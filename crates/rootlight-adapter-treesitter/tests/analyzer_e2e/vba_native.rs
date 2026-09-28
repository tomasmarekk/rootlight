//! VBA authored declarations through the production parser and normalized IR.
//! Source-bound identities are distinct from later project and runtime resolution.

use super::*;
use rootlight_ir::ContainerRef;

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
    output_with_native_kind(source, None)
}

fn output_with_native_kind(
    source: &str,
    kind: Option<rootlight_adapter_sdk::NativeVbaModuleKind>,
) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(VBA, source.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, VBA, &budget);
    let request = match kind {
        Some(kind) => request.with_native_vba_module(kind).unwrap(),
        None => request,
    };
    let result = analyze(
        &analyzer(&provider, VBA),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn vba_compilation_constants_use_a_separate_module_namespace() {
    let source = "Attribute VB_Name = \"Options\"\n#Const Enabled = True\nConst Enabled = 7\nSub Run(ByVal Enabled As Long)\n#If eNaBlEd Then\nDim value As Long\nvalue = Enabled\n#End If\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let start = u64::try_from(source.find("#Const Enabled").unwrap() + 7).unwrap();
    let definition = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition && site.source.span().start_byte() == start
        })
        .expect("compiler constant definition");
    let condition = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.compilation_identifier.reference"
                && site.source.span().start_byte()
                    == u64::try_from(source.find("eNaBlEd Then").unwrap()).unwrap()
        })
        .unwrap();
    assert_eq!(condition.target, definition.target);
    assert!(matches!(
        condition.target,
        OccurrenceTarget::Resolved { .. }
    ));
    let runtime = reference_at(document, source, "Enabled\n#End");
    assert_ne!(runtime.target, condition.target);
    assert!(matches!(runtime.target, OccurrenceTarget::Resolved { .. }));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "vba-conditional-compilation-evaluation-unavailable" })
    );
}

#[test]
fn vba_compilation_constants_include_all_directives_without_selecting_branches() {
    for source in [
        "#Const Enabled = True\n#If ENABLED Then\n#End If\n",
        "#If ENABLED Then\n#End If\n#Const Enabled = True\n",
        "#If Unknown Then\n#Const Enabled = True\n#End If\n#If ENABLED Then\n#End If\n",
        "Sub Setup()\n#Const Enabled = True\nEnd Sub\n#If ENABLED Then\n#End If\n",
        "#Const Enabled% = True\n#If ENABLED Then\n#End If\n",
        "#Const Enabled = True\n#Const Other = ENABLED\n",
        "#Const Enabled = True\n#If ENABLED Then\nSub Run(ByVal value As Long)\n#Else\nSub Run(ByVal value As Integer)\n#End If\nEnd Sub\n",
        "#Const Enabled = True\n#If Unknown Then\nSub Run(ByVal value As Long)\n#ElseIf ENABLED Then\nSub Run(ByVal value As Integer)\n#End If\nEnd Sub\n",
    ] {
        let result = output(source);
        let document = result.document();
        let reference = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.compilation_identifier.reference"
                    && site.source.span().start_byte()
                        == u64::try_from(source.find("ENABLED").unwrap()).unwrap()
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = reference.target else {
            panic!("{source}")
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Constant);
        assert_eq!(entity.visibility, rootlight_ir::EntityVisibility::Private);
        if let Some(ContainerRef::Entity(owner)) = entity.container {
            assert!(
                document
                    .entities
                    .iter()
                    .any(|entity| entity.id == owner && entity.kind == EntityKind::Module)
            );
        }
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.object == RelationEndpoint::Entity(symbol)
                && edge.evidence.source.as_ref() == Some(&reference.source)
        }));
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| { gap.detail == "vba-conditional-compilation-evaluation-unavailable" })
        );
    }
}

#[test]
fn vba_compilation_constants_do_not_bind_runtime_or_duplicate_names() {
    for source in [
        "Const Enabled = True\n#If ENABLED Then\n#End If\n",
        "#If ENABLED Then\n#End If\n",
        "#Const Enabled = True\n#Const enabled = False\n#If ENABLED Then\n#End If\n",
        "#If Unknown Then\n#Const Enabled = True\n#Else\n#Const Enabled = False\n#End If\n#If ENABLED Then\n#End If\n",
    ] {
        let result = output(source);
        let reference = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.compilation_identifier.reference"
                    && site.source.span().start_byte()
                        == u64::try_from(source.find("ENABLED").unwrap()).unwrap()
            })
            .unwrap();
        assert!(
            matches!(reference.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.source == reference.source
                && gap.detail == "vba-compilation-constant-target-unavailable"
        }));
    }
    let source =
        "#Const Enabled = True\nSub Run()\nDim output As Long\noutput = Enabled\nEnd Sub\n";
    let result = output(source);
    assert!(matches!(
        reference_at(result.document(), source, "Enabled\nEnd").target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_procedure_pointers_bind_without_calling_or_reading_locals() {
    // MS-VBAL 5.6.10 skips the procedure-local tier in pointer context.
    for (declaration, body, target_name) in [
        (
            "Private Sub Notify(ByVal value As Long)\nEnd Sub\n",
            "Dim Notify As Long\nAccept AddressOf nOtIfY\n",
            "Notify",
        ),
        (
            "Private Function Notify(ByVal value As Long) As Long\nAccept AddressOf [Notify]\nEnd Function\n",
            "",
            "Notify",
        ),
        (
            "Private Property Get Notify(ByVal value As Long) As Long\nEnd Property\nPrivate Property Let Notify(ByVal value As Long, ByVal nextValue As Long)\nEnd Property\n",
            "Accept AddressOf Notify\n",
            "Notify",
        ),
    ] {
        let source = format!("{declaration}Sub Run()\n{body}End Sub\n");
        let result = output(&source);
        let document = result.document();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.procedure_pointer.reference")
            .expect("native procedure pointer occurrence");
        let target = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == target_name
                    && !matches!(entity.kind, rootlight_ir::EntityKind::Variable)
                    && document.occurrences.iter().any(|definition| {
                        definition.role == OccurrenceRole::Definition
                            && definition.source.span().start_byte()
                                == u64::try_from(source.find(target_name).unwrap()).unwrap()
                            && definition.target == OccurrenceTarget::Resolved { symbol: entity.id }
                    })
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{source}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == rootlight_ir::RelationPredicate::RefersTo
                && edge.object == rootlight_ir::RelationEndpoint::Entity(target.id)
                && edge.evidence.source.as_ref() == Some(&site.source)
        }));
        assert!(!document.relations.iter().any(|edge| {
            edge.predicate == rootlight_ir::RelationPredicate::Calls
                && edge.evidence.source.as_ref() == Some(&site.source)
        }));
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.source == site.source && gap.detail == "vba-procedure-pointer-target-unavailable"
        }));
    }
}

#[test]
fn vba_procedure_pointers_preserve_unproven_targets() {
    for declaration in [
        "Dim Notify As Long\n",
        "Declare Sub Notify Lib \"callbacks\" ()\n",
        "Private Property Let Notify(ByVal value As Long)\nEnd Property\n",
        "Sub Notify()\nEnd Sub\nSub Notify()\nEnd Sub\n",
        "#If FEATURE Then\nSub Notify()\nEnd Sub\n#End If\n",
        "VERSION 1.0 CLASS\nAttribute VB_Name = \"Listener\"\nSub Notify()\nEnd Sub\n",
        "",
    ] {
        let source = format!("{declaration}Sub Run()\nAccept AddressOf Notify\nEnd Sub\n");
        let result = output(&source);
        let document = result.document();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.procedure_pointer.reference")
            .expect("native procedure pointer occurrence");
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.source == site.source
                    && gap.detail == "vba-procedure-pointer-target-unavailable"
            }),
            "{source}"
        );
    }
}

#[test]
fn vba_procedure_pointers_do_not_bind_partial_or_type_suffixed_names() {
    for expression in [
        "Notify.Other",
        "Notify .Other",
        "Notify _\n.Other",
        "Notify$",
        "Notify!Other",
    ] {
        let source =
            format!("Sub Notify()\nEnd Sub\nSub Run()\nAccept AddressOf {expression}\nEnd Sub\n");
        let result = output(&source);
        let document = result.document();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                matches!(
                    site.syntax_kind.as_str(),
                    "vba.procedure_pointer.reference"
                        | "vba.qualified_procedure_pointer.reference"
                        | "vba.unsupported_procedure_pointer.reference"
                )
            })
            .expect("native pointer target retained");
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{expression}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.source == site.source
                    && gap.detail == "vba-procedure-pointer-target-unavailable"
            }),
            "{expression}"
        );
    }
}

#[test]
fn vba_qualified_procedure_pointers_preserve_exact_targets_and_argument_forms() {
    for target in [
        "Callbacks.Notify",
        "[Callbacks].[Notify]",
        "cAlLbAcKs.nOtIfY",
        "Callbacks _\n.Notify",
        "Callbacks. _\r\nNotify",
    ] {
        for call in [
            format!("Accept AddressOf {target}"),
            format!("Call Accept(AddressOf {target})"),
            format!("value = Accept(AddressOf {target})"),
        ] {
            let source = format!(
                "Attribute VB_Name = \"Callbacks\"\nSub Notify()\nEnd Sub\nSub Run()\nDim Callbacks As Long\n{call}\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let site = document
                .occurrences
                .iter()
                .find(|site| site.syntax_kind == "vba.qualified_procedure_pointer.reference")
                .unwrap_or_else(|| panic!("complete target absent: {call}"));
            let symbol = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "Notify")
                .unwrap()
                .id;
            assert_eq!(site.target, OccurrenceTarget::Resolved { symbol }, "{call}");
            assert_eq!(site.role, OccurrenceRole::Reference);
            let start = usize::try_from(site.source.span().start_byte()).unwrap();
            let end = usize::try_from(site.source.span().end_byte()).unwrap();
            assert_eq!(source.get(start..end), Some(target));
            assert!(document.relations.iter().any(|edge| {
                edge.predicate == rootlight_ir::RelationPredicate::RefersTo
                    && edge.evidence.source.as_ref() == Some(&site.source)
                    && edge.object == rootlight_ir::RelationEndpoint::Entity(symbol)
            }));
            assert!(
                !document.occurrences.iter().any(|other| {
                    other.id != site.id
                        && other.source.span().start_byte() >= site.source.span().start_byte()
                        && other.source.span().end_byte() <= site.source.span().end_byte()
                }),
                "counterfeit target fragment: {call}"
            );
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == site.source
                        && gap.detail == "vba-procedure-pointer-target-unavailable")
            );
        }
    }
}

#[test]
fn vba_pointer_normalization_preserves_real_argument_calls() {
    let source = "Attribute VB_Name = \"Callbacks\"\nSub Notify()\nEnd Sub\nFunction ReadValue() As Long\nEnd Function\nSub Run()\nAccept AddressOf Callbacks.Notify, ReadValue()\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let symbol = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "ReadValue")
        .unwrap()
        .id;
    let start = u64::try_from(source.rfind("ReadValue()").unwrap()).unwrap();
    assert!(
        document
            .occurrences
            .iter()
            .any(|site| site.source.span().start_byte() == start
                && site.target == OccurrenceTarget::Resolved { symbol }
                && site.role == OccurrenceRole::CallSite)
    );
    let pointer = document
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "vba.qualified_procedure_pointer.reference")
        .unwrap();
    assert!(matches!(pointer.target, OccurrenceTarget::Resolved { .. }));
    assert!(
        !document
            .occurrences
            .iter()
            .any(
                |site| site.source.span().start_byte() > pointer.source.span().start_byte()
                    && site.source.span().start_byte() < pointer.source.span().end_byte()
            )
    );
}

#[test]
fn vba_qualified_procedure_pointers_preserve_namespace_and_target_barriers() {
    for (header, declaration, target) in [
        ("", "Sub Notify()\nEnd Sub\n", "Callbacks.Notify"),
        (
            "Attribute VB_Name = \"Other\"\n",
            "Sub Notify()\nEnd Sub\n",
            "Callbacks.Notify",
        ),
        (
            "Attribute VB_Name = \"Callbacks\"\n",
            "Dim Notify As Long\n",
            "Callbacks.Notify",
        ),
        (
            "Attribute VB_Name = \"Callbacks\"\n",
            "Declare Sub Notify Lib \"native\" ()\n",
            "Callbacks.Notify",
        ),
        (
            "Attribute VB_Name = \"Callbacks\"\n",
            "Property Get Notify() As Long\nEnd Property\n",
            "Callbacks.Notify",
        ),
        (
            "Attribute VB_Name = \"Callbacks\"\n",
            "Sub Notify()\nEnd Sub\nSub Callbacks()\nEnd Sub\n",
            "Callbacks.Notify",
        ),
        (
            "Attribute VB_Name = \"Callbacks\"\n",
            "#If Enabled Then\nSub Notify()\nEnd Sub\n#End If\n",
            "Callbacks.Notify",
        ),
        (
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Callbacks\"\n",
            "Sub Notify()\nEnd Sub\n",
            "Callbacks.Notify",
        ),
        (
            "Attribute VB_Name = \"Callbacks\"\n",
            "Sub Notify()\nEnd Sub\n",
            "Project.Callbacks.Notify",
        ),
    ] {
        let source =
            format!("{header}{declaration}Sub Run()\nCall Accept(AddressOf {target})\nEnd Sub\n");
        let result = output(&source);
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.qualified_procedure_pointer.reference")
            .expect("whole qualified target retained");
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.source == site.source
                    && gap.detail == "vba-procedure-pointer-target-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn vba_pointer_recovery_never_hides_unrelated_or_real_expression_errors() {
    for body in [
        "Call Accept(AddressOf Callbacks.Notify)\n@\n",
        "Call Accept(AddressOf Callbacks.Notify + 1)\n",
        "Call Accept(AddressOf Callbacks.Notify(1))\n",
    ] {
        let source = format!(
            "Attribute VB_Name = \"Callbacks\"\nSub Notify()\nEnd Sub\nSub Run()\n{body}End Sub\n"
        );
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
        assert_ne!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete,
            "{body}"
        );
        assert!(
            parsed
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code().as_str() == "syntax-error-recovery"),
            "{body}"
        );
    }
}

#[test]
fn vba_procedure_pointer_continuations_do_not_consume_following_statements() {
    for body in [
        "Accept AddressOf Notify _\n, 1\n",
        "Accept AddressOf Notify _\r\n, 1\r\n",
        "With receiver\nAccept AddressOf Notify\n.Value = 1\nEnd With\n",
    ] {
        let source = format!("Sub Notify()\nEnd Sub\nSub Run()\n{body}End Sub\n");
        let result = output(&source);
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.procedure_pointer.reference")
            .expect("complete simple pointer target");
        assert!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            "{body}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
    }
}

#[test]
fn vba_procedure_pointers_replay_with_native_module_context() {
    assert_vba_pointer_replay(
        "Attribute VB_Name = \"Callbacks\"\nSub Notify()\nEnd Sub\nSub Run()\nAccept AddressOf Notify\nEnd Sub\n",
        "vba.procedure_pointer.reference",
    );
}

#[test]
fn vba_qualified_procedure_pointers_replay_with_native_module_context() {
    assert_vba_pointer_replay(
        "Attribute VB_Name = \"Callbacks\"\nSub Notify()\nEnd Sub\nSub Run()\nCall Accept(AddressOf Callbacks.Notify)\nEnd Sub\n",
        "vba.qualified_procedure_pointer.reference",
    );
}

fn assert_vba_pointer_replay(source: &str, syntax: &str) {
    use rootlight_adapter_sdk::NativeVbaModuleKind;

    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, VBA);
    let fixture = Fixture::new(VBA, source.as_bytes());
    let budget = limits();
    for kind in [NativeVbaModuleKind::Procedural, NativeVbaModuleKind::Object] {
        let initial = request(&fixture.snapshot, &fixture.source, VBA, &budget)
            .with_native_vba_module(kind)
            .unwrap();
        let (first, artifact) = analyzer
            .analyze_and_capture(
                &initial,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let site = first
            .document()
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == syntax)
            .unwrap();
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            kind == NativeVbaModuleKind::Procedural
        );
        let changed = fixture.next_generation();
        let next = request(&changed.snapshot, &changed.source, VBA, &budget)
            .with_native_vba_module(kind)
            .unwrap();
        let replay = analyzer
            .analyze_from_artifact(
                &next,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let (fresh, _) = analyzer
            .analyze_and_capture(
                &next,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(replay.document(), fresh.document());
        assert_eq!(replay.report(), fresh.report());
        assert!(
            replay
                .document()
                .occurrences
                .iter()
                .all(|site| { site.source.generation() == changed.source.generation() })
        );
    }
}

#[test]
fn vba_native_context_binds_artifacts_and_required_fact_budgets() {
    use rootlight_adapter_sdk::NativeVbaModuleKind;
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, VBA);
    let fixture = Fixture::new(
        VBA,
        b"Attribute VB_Name = \"Counter\"\nPrivate total As Long\nPublic Sub Run()\n#If Enabled Then\nGoTo Finish\nFinish:\nGoTo 10\n10: Me.total = 1\n#End If\nEnd Sub\n",
    );
    let budget = limits();
    let extensions = ExtensionSupport::default();
    let contexts = [
        None,
        Some(NativeVbaModuleKind::Procedural),
        Some(NativeVbaModuleKind::Object),
    ];
    for kind in contexts {
        let initial = request(&fixture.snapshot, &fixture.source, VBA, &budget);
        let initial = match kind {
            Some(kind) => initial.with_native_vba_module(kind).unwrap(),
            None => initial,
        };
        let (_, artifact) = analyzer
            .analyze_and_capture(
                &initial,
                extensions.clone(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let demand = provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap();
        assert_eq!(
            demand,
            artifact.required_syntax_fact_count(&deadline()).unwrap()
        );
        let successor = fixture.next_generation();
        for next_kind in contexts {
            let next = request(&successor.snapshot, &successor.source, VBA, &budget);
            let next = match next_kind {
                Some(kind) => next.with_native_vba_module(kind).unwrap(),
                None => next,
            };
            let replay = analyzer.analyze_from_artifact(
                &next,
                &artifact,
                extensions.clone(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            );
            if next_kind == kind {
                let replay = replay.unwrap();
                let fresh = analyze(&analyzer, &next, &extensions);
                assert_eq!(replay.document(), fresh.document());
                assert_eq!(replay.report(), fresh.report());
            } else {
                assert!(
                    replay.is_err(),
                    "changed module category must invalidate artifacts"
                );
            }
        }
        let bounded = limits_with_syntax_records(demand);
        let request = request(&fixture.snapshot, &fixture.source, VBA, &bounded);
        let request = match kind {
            Some(kind) => request.with_native_vba_module(kind).unwrap(),
            None => request,
        };
        let (fresh, bounded_artifact) = analyzer
            .analyze_and_capture(
                &request,
                extensions.clone(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            demand,
            bounded_artifact
                .required_syntax_fact_count(&deadline())
                .unwrap()
        );
        let replay = analyzer
            .analyze_from_artifact(
                &request,
                &bounded_artifact,
                extensions.clone(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(fresh.document(), replay.document());
    }
}

#[test]
fn vba_preprocessor_branches_keep_duplicate_private_types_source_distinct() {
    let source = "Attribute VB_Name = \"Options\"\n#If VBA7 Then\nPrivate Type Configuration\nValue As Long\nEnd Type\n#Else\nPrivate Type Configuration\nValue As Integer\nEnd Type\n#End If\n";
    let result = output_with_native_kind(
        source,
        Some(rootlight_adapter_sdk::NativeVbaModuleKind::Procedural),
    );
    let document = result.document();
    let types: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Struct && entity.canonical_name == "Configuration"
        })
        .collect();
    assert_eq!(types.len(), 2);
    assert_ne!(types[0].id, types[1].id);
    let mut actual_starts = types
        .iter()
        .map(|entity| entity.evidence.source.as_ref().unwrap().span().start_byte())
        .collect::<Vec<_>>();
    actual_starts.sort_unstable();
    assert_eq!(
        actual_starts,
        source
            .match_indices("Private Type Configuration")
            .map(|(offset, _)| u64::try_from(offset).unwrap())
            .collect::<Vec<_>>()
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .all(|gap| gap.domain != FactDomain::Entities),
        "{:?}",
        document.skipped_regions
    );
}

#[test]
fn vba_nested_preprocessor_branches_preserve_private_type_inventory() {
    let source = "Attribute VB_Name = \"Options\"\n#If PLATFORM Then\n#If VBA7 Then\nPrivate Type Configuration\nValue As Long\nEnd Type\n#Else\nPrivate Type Configuration\nValue As Integer\nEnd Type\n#End If\n#End If\n";
    let result = output_with_native_kind(
        source,
        Some(rootlight_adapter_sdk::NativeVbaModuleKind::Procedural),
    );
    let document = result.document();
    let types: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Struct && entity.canonical_name == "Configuration"
        })
        .collect();
    assert_eq!(types.len(), 2);
    assert_ne!(types[0].id, types[1].id);
    assert!(
        document
            .skipped_regions
            .iter()
            .all(|gap| gap.domain != FactDomain::Entities)
    );
}

#[test]
fn vba_enum_members_bind_in_module_and_qualified_value_contexts() {
    let source = "Private Enum Phase\nReady = 1\nDone = Ready + 1\nEnd Enum\nSub Run(Optional ByVal selected As Long = Done)\nDim answer As Long\nanswer = Ready\nanswer = Phase.Done\nanswer = [Phase].[Ready]\nanswer = Phase.Done(1)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    for (text, name, ordinal) in [
        ("Ready", "Ready", 1),
        ("Done", "Done", 1),
        ("Ready", "Ready", 2),
        ("Phase.Done", "Done", 0),
        ("[Phase].[Ready]", "Ready", 0),
        ("Phase.Done", "Done", 1),
    ] {
        let start = source.match_indices(text).nth(ordinal).unwrap().0;
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + text.len()).unwrap()
                    && site.syntax_kind != "vba.application.reference"
            })
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == rootlight_ir::EntityKind::Constant && entity.canonical_name == name
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{text} at {start}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == rootlight_ir::RelationPredicate::RefersTo
                && edge.object == rootlight_ir::RelationEndpoint::Entity(target.id)
                && edge.evidence.source.as_ref() == Some(&site.source)
        }));
    }
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.predicate != rootlight_ir::RelationPredicate::Calls)
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "vba-call-or-index-target-unavailable")
    );
}

#[test]
fn vba_enum_members_preserve_shadowing_duplicates_and_initializer_order() {
    for (source, text, ordinal, target_kind, resolved) in [
        (
            "Enum Phase\nReady = 1\nEnd Enum\nSub Run()\nDim Ready As Long\nanswer = Ready\nEnd Sub\n",
            "Ready",
            2,
            rootlight_ir::EntityKind::Variable,
            true,
        ),
        (
            "Enum Phase\nReady = 1\nEnd Enum\nSub Run(ByVal Phase As Object)\nanswer = Phase.Ready\nEnd Sub\n",
            "Phase.Ready",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = 1\nEnd Enum\nSub Run()\nanswer = Phase.Ready.Other\nEnd Sub\n",
            "Phase.Ready.Other",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = 1\nready = 2\nEnd Enum\nSub Run()\nanswer = Phase.Ready\nEnd Sub\n",
            "Phase.Ready",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = 1\nEnd Enum\nEnum Phase\nDone = 2\nEnd Enum\nSub Run()\nanswer = Phase.Ready\nEnd Sub\n",
            "Phase.Ready",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = Ready + 1\nEnd Enum\n",
            "Ready",
            1,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = Done\nDone = 2\nEnd Enum\n",
            "Done",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = Phase.Done\nDone = 2\nEnd Enum\n",
            "Phase.Done",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = 1\nEnd Enum\nEnum Result\nDone = Phase.Ready\nEnd Enum\n",
            "Phase.Ready",
            0,
            rootlight_ir::EntityKind::Constant,
            true,
        ),
        (
            "#If FEATURE Then\nEnum Phase\nReady = 1\nEnd Enum\n#End If\nSub Run()\nanswer = Ready\nEnd Sub\n",
            "Ready",
            1,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = 1\nEnd Enum\nPrivate Ready As Long\nSub Run()\nanswer = Ready\nEnd Sub\n",
            "Ready",
            2,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum First\nReady = 1\nEnd Enum\nEnum Second\nReady = 2\nEnd Enum\nSub Run()\nanswer = Ready\nEnd Sub\n",
            "Ready",
            2,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Worker\"\nPrivate Enum Phase\nReady = 1\nEnd Enum\nSub Run()\nanswer = Me.Ready\nEnd Sub\n",
            "Ready",
            1,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = 1\nEnd Enum\nSub Run()\nanswer = Ready$\nEnd Sub\n",
            "Ready$",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
    ] {
        let result = output(source);
        let start = source.match_indices(text).nth(ordinal).unwrap().0;
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + text.len()).unwrap()
                    && site.syntax_kind != "vba.application.reference"
            })
            .unwrap_or_else(|| panic!("missing {text} in {source}"));
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            resolved,
            "{text} in {source}"
        );
        if let OccurrenceTarget::Resolved { symbol } = site.target {
            let target = result
                .document()
                .entities
                .iter()
                .find(|entity| entity.id == symbol)
                .unwrap();
            assert_eq!(target.kind, target_kind, "{source}");
        }
        assert_ne!(site.role, OccurrenceRole::CallSite);
    }
}

#[test]
fn vba_enum_type_member_collisions_use_source_order() {
    for (declarations, expression, resolved, name) in [
        (
            "Enum Phase\nReady = 1\nEnd Enum\nEnum Extra\nPhase = 2\nEnd Enum\nEnum Other\nPhase = 3\nEnd Enum\n",
            "Phase.Ready",
            true,
            "Ready",
        ),
        (
            "Enum Extra\nPhase = 2\nEnd Enum\nEnum Phase\nReady = 1\nEnd Enum\n",
            "Phase",
            true,
            "Phase",
        ),
        (
            "Enum Extra\nPhase = 2\nEnd Enum\nEnum Other\nPhase = 3\nEnd Enum\nEnum Phase\nReady = 1\nEnd Enum\n",
            "Phase",
            false,
            "Phase",
        ),
        (
            "Enum Phase\nPhase = 1\nEnd Enum\n",
            "Phase.Phase",
            true,
            "Phase",
        ),
    ] {
        let source = format!("{declarations}Sub Run()\nanswer = {expression}\nEnd Sub\n");
        let result = output(&source);
        let start = source.rfind(expression).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte()
                        == u64::try_from(start + expression.len()).unwrap()
                    && site.syntax_kind != "vba.application.reference"
            })
            .unwrap();
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            resolved,
            "{source}"
        );
        if let OccurrenceTarget::Resolved { symbol } = site.target {
            let target = result
                .document()
                .entities
                .iter()
                .find(|entity| entity.id == symbol)
                .unwrap();
            assert_eq!(target.kind, rootlight_ir::EntityKind::Constant);
            assert_eq!(target.canonical_name, name);
        }
    }
}

#[test]
fn vba_member_invocation_context_does_not_prove_a_call() {
    let source = "Sub Run(ByVal worker As Object, ByVal other As Object)\nCall worker.Apply(1)\nworker.Apply 2\nanswer = worker.Compute(3)\nworker.Item(1) = 4\nSet worker.Item(2) = other\nanswer = worker.Item\nCall worker.[Apply](5)\nworker.Check 1 = 2\nCall worker.Check((1) = 2)\nworker.Check (1) = 2, 3\nworker.Item (1) = 4\nEnd Sub\n";
    let result = output(source);
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_vba::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    for (text, invoked, ordinal) in [
        ("worker.Apply", true, 0),
        ("worker.Apply", true, 1),
        ("worker.Compute", true, 0),
        ("worker.Item", false, 0),
        ("worker.Item", false, 1),
        ("worker.Item", false, 2),
        ("worker.[Apply]", true, 0),
        ("worker.Check", true, 0),
        ("worker.Check", true, 1),
        ("worker.Check", true, 2),
        ("worker.Item", false, 3),
    ] {
        let start = source.match_indices(text).nth(ordinal).unwrap().0;
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                usize::try_from(site.source.span().start_byte()).unwrap() == start
                    && usize::try_from(site.source.span().end_byte()).unwrap() == start + text.len()
                    && site.syntax_kind != "vba.application.reference"
            })
            .unwrap();
        assert_eq!(
            site.syntax_kind,
            if invoked {
                "vba.invoked_member.reference"
            } else {
                "vba.member.reference"
            },
            "{text} at {start}: {}",
            tree.root_node().to_sexp()
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.source == site.source && gap.detail == "vba-member-target-unavailable"
        }));
    }
    assert!(
        result
            .document()
            .relations
            .iter()
            .all(|relation| { relation.predicate != rootlight_ir::RelationPredicate::Calls })
    );
}

#[test]
fn vba_explicit_declaration_visibility_survives_lowering() {
    use rootlight_ir::EntityVisibility;
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Boundary\"\nPublic exposed As Long, another As Long\nPrivate hidden As Long\nPublic Sub Visible()\nEnd Sub\nPrivate Sub Internal()\nEnd Sub\nFriend Sub ProjectOnly()\nEnd Sub\nPrivate Type Coordinate\nX As Long\nEnd Type\n";
    let result = output(source);
    for (name, expected) in [
        ("exposed", EntityVisibility::Public),
        ("another", EntityVisibility::Public),
        ("hidden", EntityVisibility::Private),
        ("Visible", EntityVisibility::Public),
        ("Internal", EntityVisibility::Private),
        ("ProjectOnly", EntityVisibility::Restricted),
        ("Coordinate", EntityVisibility::Private),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.visibility, expected, "{name}");
    }
}

#[test]
fn vba_visibility_does_not_infer_defaults_from_body_text() {
    use rootlight_ir::EntityVisibility;
    let source = "Attribute VB_Name = \"Boundary\"\n' Public\nDim stored As Long\nSub Run(ByVal value As Long)\nDim text As String\ntext = \"Private Public Friend\"\nEnd Sub\nFriend Sub InvalidFriend()\nEnd Sub\n";
    let result = output(source);
    for name in ["stored", "Run", "value", "text", "InvalidFriend"] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.visibility, EntityVisibility::Unknown, "{name}");
    }
}

#[test]
fn vba_instance_udt_fields_follow_class_ownership() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate Type Coordinate\nX As Long\nEnd Type\nPrivate Type Box\nLocation As Coordinate\nEnd Type\nPrivate position As Box\nSub Run(ByVal position As Variant)\nMe.position.Location.X = 1\nmE.[position].Location.X = 2\nEnd Sub\n";
    let result = output(source);
    for expression in [
        "Me.position.Location",
        "Me.position.Location.X",
        "mE.[position].Location.X",
    ] {
        let name = expression.rsplit('.').next().unwrap();
        let target = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == name)
            .unwrap();
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.member.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte()
                        == start + u64::try_from(expression.len()).unwrap()
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{expression}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(
            !result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-member-target-unavailable"
                    && gap.source.span() == site.source.span())
        );
    }
}

#[test]
fn vba_instance_udt_fields_preserve_unproven_receiver_barriers() {
    for (header, declaration) in [
        ("", "Private position As Coordinate\n"),
        ("VERSION 1.0 CLASS\n", "Private position As Variant\n"),
        ("VERSION 1.0 CLASS\n", "Private position() As Coordinate\n"),
        (
            "VERSION 1.0 CLASS\n",
            "Private position As Coordinate\nPrivate POSITION As Coordinate\n",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "#If FLAG Then\nPrivate position As Coordinate\n#Else\nPrivate position As Variant\n#End If\n",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Property Get position() As Coordinate\nEnd Property\n",
        ),
    ] {
        let source = format!(
            "{header}Attribute VB_Name = \"Holder\"\nPrivate Type Coordinate\nX As Long\nEnd Type\n{declaration}Sub Run()\nDim position As Coordinate\nMe.position.X = 1\nEnd Sub\n"
        );
        let result = output(&source);
        let expression = "Me.position.X";
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.member.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte()
                        == start + u64::try_from(expression.len()).unwrap()
            })
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-member-target-unavailable"
                    && gap.source.span() == site.source.span())
        );
    }
}

#[test]
fn vba_declared_udt_fields_follow_local_types_and_nested_fields() {
    let source = "Private Type Coordinate\nX As Long\nEnd Type\nPrivate Type Box\nLocation As Coordinate\nEnd Type\nSub Run(ByRef value As Box)\nDim snapshot As Coordinate\nsnapshot.X = value.Location.X\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    for (expression, field) in [
        ("snapshot.X", "X"),
        ("value.Location", "Location"),
        ("value.Location.X", "X"),
    ] {
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == field)
            .unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.member.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte()
                        == start + u64::try_from(expression.len()).unwrap()
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{expression}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.detail == "vba-member-target-unavailable" && gap.source.span() == site.source.span()
        }));
    }
}

#[test]
fn vba_declared_type_references_ignore_value_namespace_shadowing() {
    let source = "Private Type Coordinate\nX As Long\nEnd Type\nSub Run(ByVal Coordinate As Long)\nDim point As Coordinate\npoint.X = Coordinate\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Struct && entity.canonical_name == "Coordinate")
        .unwrap();
    let start = u64::try_from(source.find("As Coordinate").unwrap() + 3).unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| site.source.span().start_byte() == start)
        .unwrap();
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert_eq!(site.role, OccurrenceRole::TypeUse);
    assert!(
        document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::UsesType
                && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id)
                && relation.evidence.source.as_ref() == Some(&site.source))
    );
}

#[test]
fn vba_type_tests_reference_udts_without_declaring_function_return_types() {
    let source = "Private Type Shape\nX As Long\nEnd Type\nPrivate candidate As Variant\nFunction Item() As Object\nIf TypeOf candidate Is Shape Then\nEnd If\nEnd Function\nSub Run()\nItem().X = 1\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Struct && entity.canonical_name == "Shape")
        .unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "vba.tested_type_identifier.reference")
        .unwrap();
    assert_eq!(site.role, OccurrenceRole::TypeUse);
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert!(
        document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::UsesType
                && edge.object == rootlight_ir::RelationEndpoint::Entity(target.id)
                && edge.evidence.source.as_ref() == Some(&site.source))
    );
    let member = document
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "vba.member.reference")
        .unwrap();
    assert!(matches!(member.target, OccurrenceTarget::Unresolved { .. }));
}

#[test]
fn vba_construction_types_never_bind_to_values_or_udts() {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_vba::LANGUAGE.into())
        .unwrap();
    for declaration in [
        "",
        "Private Type Worker\nValue As Long\nEnd Type\n",
        "Private Enum Worker\nReady = 1\nEnd Enum\n",
    ] {
        let source = format!(
            "{declaration}Sub Run(ByVal Worker As Long)\nDim value As Object\nSet value = New Worker\nSet value = New [Worker]\nSet value = New Missing.Worker\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let types: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind.contains("constructed_type_identifier"))
            .collect();
        assert!(types.len() >= 3, "{:#?}", document.occurrences);
        for site in types {
            assert_eq!(site.role, OccurrenceRole::TypeUse);
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert!(document.skipped_regions.iter().any(
                |gap| gap.source == site.source && gap.detail == "vba-type-target-unavailable"
            ));
        }
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| site.syntax_kind == "vba.construction.reference")
                .count(),
            3
        );
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "vba.member.reference"),
            "{}",
            parser.parse(&source, None).unwrap().root_node().to_sexp()
        );
    }
}

#[test]
fn vba_declared_udt_members_preserve_ambiguous_dynamic_and_array_barriers() {
    for (types, globals, parameter, body, unresolved) in [
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point As Variant",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point As Object",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point As External",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point() As Coordinate",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point As New Coordinate",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nx As Long\nEnd Type\n",
            "",
            "point As Coordinate",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\nPrivate Type Coordinate\nY As Long\nEnd Type\n",
            "",
            "point As Coordinate",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\n#If FLAG Then\nX As Long\n#Else\nX As Long\n#End If\nEnd Type\n",
            "",
            "point As Coordinate",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "Private point As Coordinate\n",
            "point As Variant",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\nPrivate Type Box\nLocation() As Coordinate\nEnd Type\n",
            "",
            "value As Box",
            "value.Location.X = 1",
            "value.Location.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point As Coordinate",
            "point.X$ = 1",
            "point.X$",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point As Coordinate",
            "point!X = 1",
            "point!X",
        ),
    ] {
        let source = format!("{types}{globals}Sub Run(ByRef {parameter})\n{body}\nEnd Sub\n");
        let result = output(&source);
        let start = u64::try_from(source.find(unresolved).unwrap()).unwrap();
        let end = start + u64::try_from(unresolved.len()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.member.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-member-target-unavailable"
                    && gap.source.span() == site.source.span()),
            "{source}"
        );
    }
}

#[test]
fn vba_declared_udt_field_references_do_not_invent_calls() {
    let source = "Private Type Coordinate\nX As Long\nEnd Type\nSub Run(ByRef point As Coordinate)\nCall point.X(1)\nEnd Sub\n";
    let result = output(source);
    let field = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "X")
        .unwrap();
    let site = result
        .document()
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "vba.invoked_member.reference")
        .unwrap();
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: field.id });
    assert_eq!(site.role, OccurrenceRole::Reference);
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "vba-call-or-index-target-unavailable")
    );
}

#[test]
fn vba_declared_udt_types_and_fields_survive_generation_replay() {
    let source = "Private Type Coordinate\r\n[X] As Long\r\nEnd Type\r\nSub Run(ByRef point As _\r\n Coordinate)\r\npoint.[X] = 1\r\nEnd Sub\r\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, VBA);
    let fixture = Fixture::new(VBA, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, VBA, &budget);
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let field = first
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "X")
        .unwrap();
    let site = first
        .document()
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "vba.member.reference")
        .unwrap();
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: field.id });
    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, VBA, &budget);
    let replay = analyzer
        .analyze_from_artifact(
            &next,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let (fresh, _) = analyzer
        .analyze_and_capture(
            &next,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(replay.document(), fresh.document());
    assert_eq!(replay.report(), fresh.report());
    assert!(
        replay
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.generation() == changed.source.generation())
    );
}

#[test]
fn vba_declared_udt_fields_do_not_rebind_instance_keywords() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate Type Coordinate\nX As Long\nEnd Type\nPrivate Type Box\nLocation As Coordinate\nEnd Type\nPrivate Location As Variant\nSub Run()\nDim [Me] As Box\nMe.Location.X = 1\n[Me].Location.X = 2\nEnd Sub\n";
    let result = output(source);
    let field = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "X")
        .unwrap();
    for (expression, resolved) in [("Me.Location.X", false), ("[Me].Location.X", true)] {
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.member.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        if resolved {
            assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: field.id });
        } else {
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{expression}"
            );
            assert!(
                result
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "vba-member-target-unavailable"
                        && gap.source.span() == site.source.span())
            );
        }
    }
}

#[test]
fn vba_declared_udt_fields_remain_unresolved_when_capture_budget_is_clipped() {
    let source = format!(
        "Private Type Coordinate\nX As Long\nEnd Type\nSub Run(ByRef point As Coordinate)\npoint.X = 1\n{}End Sub\n",
        "unknown = another\n".repeat(100)
    );
    let provider = Arc::new(provider());
    let fixture = Fixture::new(VBA, source.as_bytes());
    let defaults = limits();
    let initial = request(&fixture.snapshot, &fixture.source, VBA, &defaults);
    let demand = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let budget = limits_with_syntax_records(demand + 8);
    let result = analyze(
        &analyzer(&provider, VBA),
        &request(&fixture.snapshot, &fixture.source, VBA, &budget),
        &ExtensionSupport::default(),
    );
    assert!(
        result
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-extraction-limit")
    );
    let members: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "vba.member.reference")
        .collect();
    assert!(!members.is_empty());
    assert!(
        members
            .iter()
            .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn visual_basic_classic_preserves_structure_and_dialect_uncertainty() {
    let case = LanguageCase {
        name: "visual-basic-classic",
        ..VBA
    };
    let source = "Attribute VB_Name = \"Meter\"\nPublic Function Measure(ByVal value As Long) As Long\nMeasure = value + 1\nEnd Function\n";
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    for name in ["Meter", "Measure", "value"] {
        assert!(
            document.entities.iter().any(|entity| {
                entity.canonical_name == name && entity.language == "visual-basic-classic"
            }),
            "missing {name}"
        );
    }
    let start = u64::try_from(source.find("value +").unwrap()).unwrap();
    let value = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "value")
        .unwrap();
    assert!(document.occurrences.iter().any(|site| {
        site.source.span().start_byte() == start
            && site.target == OccurrenceTarget::Resolved { symbol: value.id }
    }));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail == "visual-basic-dialect-unproven"
            && gap.source.span().start_byte() == 0
            && gap.source.span().end_byte() == u64::try_from(source.len()).unwrap()
    }));
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn visual_basic_classic_preserves_class_accessor_and_instance_identity() {
    let case = LanguageCase {
        name: "visual-basic-classic",
        ..VBA
    };
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate stored As Variant\nPublic Property Get Item() As Variant\nItem = stored\nEnd Property\nPublic Property Let Item(ByVal value As Variant)\nstored = value\nEnd Property\nSub Run(ByVal target As Variant)\nSet target = Me\nEnd Sub\n";
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    let class = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Holder")
        .unwrap();
    let accessors: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property && entity.canonical_name == "Item")
        .collect();
    assert_eq!(accessors.len(), 2);
    assert_ne!(accessors[0].id, accessors[1].id);
    assert!(
        document
            .entities
            .iter()
            .all(|entity| entity.language == case.name)
    );
    let start = u64::try_from(source.find("Me").unwrap()).unwrap();
    assert!(document.occurrences.iter().any(|site| {
        site.source.span().start_byte() == start
            && site.source.span().end_byte() == start + 2
            && site.target == OccurrenceTarget::Resolved { symbol: class.id }
    }));
}

#[test]
fn visual_basic_classic_partial_analysis_scopes_dialect_uncertainty() {
    let case = LanguageCase {
        name: "visual-basic-classic",
        ..VBA
    };
    let body = "Sub Run()\nEnd Sub\n";
    let source = format!("excluded prefix\n{body}excluded suffix\n");
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let start = u64::try_from(source.find(body).unwrap()).unwrap();
    let span = SourceSpan::new(
        fixture.source.span().file(),
        start,
        start + u64::try_from(body.len()).unwrap(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case),
        EncodingId::utf8(),
        vec![rootlight_adapter_sdk::IncludedRange::new(
            span,
            language(case),
        )],
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, case),
        &request,
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    let gaps: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "visual-basic-dialect-unproven")
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].source.span(), span);
    assert!(
        document
            .entities
            .iter()
            .any(|entity| { entity.language == case.name && entity.canonical_name == "Run" })
    );
}

#[test]
fn vba_instance_expressions_reference_the_source_class() {
    for statement in [
        "Set value = mE",
        "value = mE.Item",
        "Visit mE",
        "With mE\n.Item = 1\nEnd With",
        "If value Is mE Then\nvalue = 1\nEnd If",
        "value = mE(0)",
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPublic Item As Variant\nSub Run(ByVal value As Variant)\n{statement}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let class = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Holder")
            .unwrap();
        let start = u64::try_from(source.find("mE").unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 2
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: class.id },
            "{statement}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(class.id)
        }));
        assert!(!document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(site.id)
        }));
        if statement.contains("mE(0)") {
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.source.span() == site.source.span()
                    && gap.detail == "vba-call-or-index-target-unavailable"
            }));
        }
    }
}

#[test]
fn vba_instance_expressions_require_class_bodies_and_keyword_spelling() {
    for (prefix, body, suffix, marker) in [
        ("", "Sub Run()\nSet value = mE\nEnd Sub\n", "", "mE"),
        (
            "VERSION 1.0 FORM\n",
            "Sub Run()\nSet value = mE\nEnd Sub\n",
            "",
            "mE",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Sub Run(Optional value As Variant = mE)\nEnd Sub\n",
            "",
            "mE",
        ),
        (
            "VERSION 1.0 CLASS\nSub Run()\n",
            "Const value = mE\n",
            "End Sub\n",
            "mE",
        ),
        (
            "VERSION 1.0 CLASS\nSub Run()\n",
            "value = New mE\n",
            "End Sub\n",
            "mE",
        ),
        (
            "VERSION 1.0 CLASS\nSub Run()\n",
            "value = AddressOf mE\n",
            "End Sub\n",
            "mE",
        ),
        (
            "VERSION 1.0 CLASS\nSub Run()\n",
            "value = [mE]\n",
            "End Sub\n",
            "[mE]",
        ),
        (
            "VERSION 1.0 CLASS\nSub Run()\n",
            "value = mE$\n",
            "End Sub\n",
            "mE$",
        ),
    ] {
        let source = format!("{prefix}{body}{suffix}");
        let result = output(&source);
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + u64::try_from(marker.len()).unwrap()
            })
            .unwrap_or_else(|| panic!("instance-context reference is missing: {source}"));
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            result.document().skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Relations && gap.source.span() == site.source.span()
            }),
            "{source}"
        );
    }
    let source = "VERSION 1.0 CLASS\nSet value = mE\n";
    let result = output(source);
    let start = u64::try_from(source.find("mE").unwrap()).unwrap();
    let statement_start = u64::try_from(source.find("Set").unwrap()).unwrap();
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    // Recovery diagnoses the executable statement at Set, not at its value.
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Diagnostics
            && gap.reason == rootlight_ir::SkippedRegionReason::ParseError
            && gap.source.span().start_byte() == statement_start
            && gap.source.span().end_byte() == statement_start + 3
    }));
    assert!(!result.document().occurrences.iter().any(|site| {
        site.source.span().start_byte() == start
            && matches!(site.target, OccurrenceTarget::Resolved { .. })
    }));
}

#[test]
fn vba_result_references_bind_the_enclosing_function_or_getter() {
    for (header, end, kind) in [
        (
            "Function Measure(ByVal value As Variant) As Variant",
            "Function",
            EntityKind::Function,
        ),
        (
            "Property Get Measure(ByVal value As Variant) As Variant",
            "Property",
            EntityKind::Property,
        ),
    ] {
        for statement in [
            "[mEaSuRe] = value",
            "Let [mEaSuRe] = value",
            "Set [mEaSuRe] = value",
            "value = [mEaSuRe]",
            "value = 1 + [mEaSuRe]",
            "Visit [mEaSuRe]",
        ] {
            let source = format!("{header}\n{statement}\nEnd {end}\n");
            let result = output(&source);
            let document = result.document();
            let target = document
                .entities
                .iter()
                .find(|entity| entity.kind == kind && entity.canonical_name == "Measure")
                .unwrap();
            let site = reference_at(document, &source, "[mEaSuRe]");
            assert_eq!(
                site.source.span().end_byte() - site.source.span().start_byte(),
                9
            );
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{source}"
            );
            assert_eq!(site.role, OccurrenceRole::Reference, "{source}");
            assert!(document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(target.id)
            }));
            assert!(!document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
            }));
            assert!(!document.skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Relations && gap.source.span() == site.source.span()
            }));
        }
    }
}

#[test]
fn vba_result_references_keep_type_scope_and_index_barriers() {
    for (before, body, after, marker) in [
        (
            "Function Measure() As Variant\n",
            "mEaSuRe$ = 1",
            "End Function\n",
            "mEaSuRe$",
        ),
        (
            "Function Measure() As Variant\nDim Measure As Variant\n",
            "mEaSuRe = 1",
            "End Function\n",
            "mEaSuRe",
        ),
        (
            "Property Get Measure() As Variant\nDim value As Variant\n",
            "value = mEaSuRe(0)",
            "End Property\n",
            "mEaSuRe",
        ),
        (
            "Property Get Measure() As Variant\n",
            "mEaSuRe(0) = 1",
            "End Property\n",
            "mEaSuRe",
        ),
        (
            "Property Get Measure() As Variant\n",
            "Call mEaSuRe()",
            "End Property\n",
            "mEaSuRe",
        ),
        (
            "Property Get Measure() As Variant\nDim Measure As Variant\n",
            "mEaSuRe = 1",
            "End Property\n",
            "mEaSuRe",
        ),
        (
            "Function Measure(Optional value As Variant = mEaSuRe) As Variant\n",
            "",
            "End Function\n",
            "mEaSuRe",
        ),
    ] {
        let source = format!("{before}{body}\n{after}");
        let result = output(&source);
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + u64::try_from(marker.len()).unwrap()
            })
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert_ne!(site.role, OccurrenceRole::CallSite, "{source}");
        assert!(
            result.document().skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Relations && gap.source.span() == site.source.span()
            }),
            "{source}"
        );
    }
}

#[test]
fn vba_conditional_procedure_results_bind_within_their_body() {
    for (header, end, kind) in [
        (
            "Private Function Measure() As Variant",
            "Function",
            EntityKind::Function,
        ),
        (
            "Private Property Get Measure() As Variant",
            "Property",
            EntityKind::Property,
        ),
    ] {
        let source = format!("#If Mac Then\n\n{header}\n    Measure = 1\nEnd {end}\n\n#End If\n");
        let result = output(&source);
        let document = result.document();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.kind == kind && entity.canonical_name == "Measure")
            .unwrap();
        let site = reference_at(document, &source, "Measure = 1");
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.domain == FactDomain::Relations && gap.source.span() == site.source.span()
        }));
    }
}

#[test]
fn vba_conditional_procedure_result_does_not_escape_or_override_a_local() {
    let source = "#If Mac Then\n\nPrivate Function Measure() As Variant\n    Dim Measure As Variant\n    Measure = 1\nEnd Function\n\n#End If\nPublic Sub Consumer()\n    Dim result As Variant\n    result = Measure\nEnd Sub\n";
    let result = output(source);
    for marker in ["Measure = 1", "Measure\nEnd Sub"] {
        let site = reference_at(result.document(), source, marker);
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.domain == FactDomain::Relations && gap.source.span() == site.source.span()
        }));
    }
}

#[test]
fn vba_unqualified_properties_select_source_bound_accessors() {
    for (parameters, argument) in [("", ""), ("ByVal index As Long", "(0)")] {
        let setter_parameters = if parameters.is_empty() {
            "ByVal value As Variant".to_owned()
        } else {
            format!("{parameters}, ByVal value As Variant")
        };
        for (statement, accessor) in [
            (format!("value = [iTeM]{argument}"), "Get Item"),
            (format!("[iTeM]{argument} = value"), "Let Item"),
            (format!("Let [iTeM]{argument} = value"), "Let Item"),
            (format!("Set [iTeM]{argument} = value"), "Set Item"),
            (format!("value = 1 + [iTeM]{argument}"), "Get Item"),
            (format!("Visit [iTeM]{argument}"), "Get Item"),
        ] {
            let source = format!(
                "Property Get Item({parameters}) As Variant\nItem = 1\nEnd Property\nProperty Let Item({setter_parameters})\nEnd Property\nProperty Set Item({setter_parameters})\nEnd Property\nSub Run(ByVal value As Variant)\n{statement}\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let definition_start = u64::try_from(source.find(accessor).unwrap() + 4).unwrap();
            let target = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.source.span().start_byte() == definition_start
                })
                .unwrap()
                .target
                .clone();
            assert!(matches!(target, OccurrenceTarget::Resolved { .. }));
            let start = u64::try_from(source.find("[iTeM]").unwrap()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == start
                        && site.source.span().end_byte() == start + 6
                })
                .unwrap();
            assert_eq!(site.target, target, "{statement}");
            assert_eq!(site.role, OccurrenceRole::CallSite, "{statement}");
            let OccurrenceTarget::Resolved { symbol } = target else {
                unreachable!()
            };
            assert!(document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(symbol)
            }));
        }
    }
}

#[test]
fn vba_unqualified_properties_respect_local_array_proofs_and_dynamic_index_gaps() {
    for (parameter, local) in [
        ("", "Dim Item(2) As Variant\n"),
        ("ByVal Item As Variant", ""),
    ] {
        for statement in [
            "value = iTeM",
            "iTeM = value",
            "Set iTeM = value",
            "iTeM(0) = value",
            "value = iTeM(0)",
        ] {
            let source = format!(
                "Property Get Item() As Variant\nItem = 1\nEnd Property\nSub Run({parameter})\n{local}Dim value As Variant\n{statement}\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let value = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "Item"
                        && matches!(entity.kind, EntityKind::Variable | EntityKind::Parameter)
                })
                .unwrap();
            let start = u64::try_from(source.find("iTeM").unwrap()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == start
                        && site.source.span().end_byte() == start + 4
                })
                .unwrap();
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved { symbol: value.id },
                "{statement}"
            );
            assert_eq!(site.role, OccurrenceRole::Reference);
            if statement.contains('(') {
                let proven_array_access = parameter.is_empty();
                assert_eq!(
                    document.skipped_regions.iter().any(|gap| {
                        gap.domain == FactDomain::Relations
                            && gap.source.span() == site.source.span()
                            && gap.detail == "vba-call-or-index-target-unavailable"
                    }),
                    !proven_array_access,
                    "{source}"
                );
                assert!(!document.relations.iter().any(|edge| {
                    edge.predicate == RelationPredicate::Calls
                        && edge.subject == RelationEndpoint::Occurrence(site.id)
                }));
            }
        }
    }
}

#[test]
fn vba_unqualified_properties_keep_result_slots_and_unproven_calls_explicit() {
    let get = "Property Get Item() As Variant\nItem = 1\nEnd Property\n";
    for (declarations, statement) in [
        (get.to_owned(), "iTeM = value"),
        (get.to_owned(), "Set iTeM = value"),
        (get.to_owned(), "Call iTeM()"),
        (get.to_owned(), "ReDim iTeM(2)"),
        (get.to_owned(), "value = iTeM$"),
        (get.repeat(2), "value = iTeM"),
        (format!("#If Enabled Then\n{get}#End If\n"), "value = iTeM"),
        (
            "Function Item(ByVal index As Long) As Variant\nEnd Function\n".to_owned(),
            "iTeM(0) = value",
        ),
    ] {
        let source =
            format!("{declarations}Sub Run(ByVal value As Variant)\n{statement}\nEnd Sub\n");
        let result = output(&source);
        let start = u64::try_from(source.find("iTeM").unwrap()).unwrap();
        let name_length = if statement.contains("iTeM$") { 5 } else { 4 };
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + name_length
            })
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.domain == FactDomain::Relations && gap.source.span() == site.source.span()
        }));
    }
    for statement in ["iTeM = value", "Set iTeM = value", "value = iTeM"] {
        let source = format!(
            "Property Get Item() As Variant\nDim value As Variant\n{statement}\nEnd Property\nProperty Let Item(ByVal value As Variant)\nEnd Property\nProperty Set Item(ByVal value As Variant)\nEnd Property\n"
        );
        let result = output(&source);
        let document = result.document();
        let getter = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition && site.source.span().start_byte() == 13
            })
            .unwrap();
        assert!(matches!(getter.target, OccurrenceTarget::Resolved { .. }));
        assert_eq!(
            reference_at(document, &source, "iTeM").target,
            getter.target
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
fn vba_function_values_without_parentheses_accept_omittable_parameters() {
    for parameters in [
        "",
        "()",
        "(Optional ByVal input As Long = 1)",
        "(ParamArray items() As Variant)",
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Meter\"\nFunction Measure{parameters} As Long\nMeasure = 1\nEnd Function\nSub Run()\nDim Measure As Variant\nMeasure = Me.[mEaSuRe]\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let method = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Method && entity.canonical_name == "Measure")
            .unwrap();
        let start = u64::try_from(source.find("[mEaSuRe]").unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 9
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: method.id },
            "{parameters}"
        );
        assert_eq!(site.role, OccurrenceRole::CallSite, "{parameters}");
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(method.id))
        );
    }
}

#[test]
fn vba_bare_function_values_resolve_in_expression_contexts() {
    for expression in ["mEaSuRe", "1 + mEaSuRe", "Abs(mEaSuRe)"] {
        let source = format!(
            "Function Measure() As Long\nMeasure = 1\nEnd Function\nSub Run()\nDim value As Long\nvalue = {expression}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let function = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Function && entity.canonical_name == "Measure"
            })
            .unwrap();
        let site = reference_at(document, &source, "mEaSuRe");
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved {
                symbol: function.id
            }
        );
        assert_eq!(site.role, OccurrenceRole::CallSite);
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(function.id)
        }));
    }
}

#[test]
fn vba_nested_function_argument_resolves_to_local_declaration() {
    let source = "Attribute VB_Name = \"Utility\"\nPublic Function Normalize(ByVal value As Long) As Long\nNormalize = value\nEnd Function\nPublic Sub Run(ByVal input As Object, ByVal output As Object)\nIf input.Visible Then\nDebug.Print \"header\" & Format(Now, \"Long Time\")\nDebug.Print Normalize(input.Status) & \" \" & output.Render(input)\nEnd If\nEnd Sub\n".replace('\n', "\r\n");
    let result = output(&source);
    let document = result.document();
    let function = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "Normalize")
        .unwrap();
    let start = u64::try_from(source.find("Normalize(input.Status)").unwrap()).unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| {
            site.source.span().start_byte() == start
                && site.source.span().end_byte() == start + 9
                && site.syntax_kind.ends_with("identifier.reference")
        })
        .unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_vba::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(&source, None).unwrap();
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved {
            symbol: function.id
        },
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(site.role, OccurrenceRole::CallSite);
    assert!(document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Calls
            && edge.subject == RelationEndpoint::Occurrence(site.id)
            && edge.object == RelationEndpoint::Entity(function.id)
    }));
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.source.span() == site.source.span()
            && gap.detail == "vba-call-or-index-target-unavailable"
    }));
}

#[test]
fn vba_nested_index_argument_does_not_call_shadowed_function() {
    let source = "Attribute VB_Name = \"Utility\"\nPublic Function Normalize(ByVal value As Long) As Long\nNormalize = value\nEnd Function\nPublic Sub Run(ByVal input As Object, ByVal output As Object)\nDim Normalize(2) As Long\nIf input.Visible Then\nDebug.Print \"header\" & Format(Now, \"Long Time\")\nDebug.Print Normalize(0) & \" \" & output.Render(input)\nEnd If\nEnd Sub\n".replace('\n', "\r\n");
    let result = output(&source);
    let document = result.document();
    let variable = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "Normalize")
        .unwrap();
    let start = u64::try_from(source.find("Normalize(0)").unwrap()).unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| {
            site.source.span().start_byte() == start
                && site.source.span().end_byte() == start + 9
                && site.syntax_kind.ends_with("identifier.reference")
        })
        .unwrap();
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved {
            symbol: variable.id
        }
    );
    assert_eq!(site.role, OccurrenceRole::Reference);
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Calls
            && edge.subject == RelationEndpoint::Occurrence(site.id)
    }));
}

#[test]
fn vba_local_applications_bind_complete_argument_lists() {
    for (parameters, arguments) in [
        ("ByVal value As Long", "1"),
        ("ByVal value As Long, ByVal other As Long", "Inner(1, 2), 3"),
        ("ByVal value As Long, Optional other As Long = 0", "1"),
        (
            "ByVal value As Long, Optional other As Long = 0",
            "other:=2, value:=1",
        ),
        (
            "Optional first As Long = 0, Optional last As Long = 0",
            ", 2",
        ),
        ("ByVal value As Long", " _\r\n 1"),
    ] {
        for (declaration, expression) in [
            (
                format!("Sub Accept({parameters})\nEnd Sub"),
                format!("Accept {arguments}"),
            ),
            (
                format!("Function Accept({parameters}) As Long\nEnd Function"),
                format!("result = Accept({arguments})"),
            ),
        ] {
            let source =
                format!("{declaration}\nSub Run()\nDim result As Long\n{expression}\nEnd Sub\n");
            let result = output(&source);
            let document = result.document();
            let target = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "Accept")
                .unwrap();
            let start = u64::try_from(source.rfind("Accept").unwrap()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.application.reference"
                        && site.source.span().start_byte() == start
                })
                .unwrap();
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{source}"
            );
            assert_eq!(site.role, OccurrenceRole::Reference);
            assert!(
                document
                    .relations
                    .iter()
                    .any(|edge| edge.subject == RelationEndpoint::Occurrence(site.id)
                        && edge.predicate == RelationPredicate::RefersTo
                        && edge.object == RelationEndpoint::Entity(target.id))
            );
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == site.source
                        && gap.detail == "vba-call-or-index-target-unavailable")
            );
        }
    }
}

#[test]
fn vba_local_applications_reject_argument_and_dispatch_ambiguity() {
    for (declaration, invocation) in [
        ("Sub Accept(ByVal value As Long)\nEnd Sub", "Accept"),
        ("Sub Accept(ByVal value As Long)\nEnd Sub", "Accept 1, 2"),
        (
            "Sub Accept(ByVal value As Long, Optional other As Long = 0)\nEnd Sub",
            "Accept , 2",
        ),
        (
            "Sub Accept(ByVal value As Long)\nEnd Sub",
            "Accept value:=1, value:=2",
        ),
        (
            "Sub Accept(ByVal value As Long)\nEnd Sub",
            "Accept 1, value:=2",
        ),
        (
            "Sub Accept(ByVal value As Long)\nEnd Sub",
            "Accept missing:=1",
        ),
        ("Sub Accept(ByVal value As Long)\nEnd Sub", "Accept ByVal 1"),
        (
            "Sub Accept(ByVal value As Long, ByVal VALUE As Long)\nEnd Sub",
            "Accept 1, 2",
        ),
        (
            "Sub Accept(ByVal value As Long)\nEnd Sub\nSub Accept(ByVal value As Long)\nEnd Sub",
            "Accept 1",
        ),
        (
            "Function Accept() As Object\nEnd Function",
            "result = Accept()(1)",
        ),
        (
            "Sub Accept(ByVal value As Long)\nEnd Sub",
            "Dim Accept As Object\nAccept 1",
        ),
    ] {
        let source =
            format!("{declaration}\nSub Run()\nDim result As Variant\n{invocation}\nEnd Sub\n");
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.rfind("Accept").unwrap()).unwrap();
        let application = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
            })
            .max_by_key(|site| site.source.span().end_byte())
            .unwrap();
        assert!(
            matches!(application.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == application.source
                    && gap.detail == "vba-call-or-index-target-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn vba_explicit_calls_and_call_prefixed_names_keep_exact_application_sources() {
    for invocation in [
        "Call Callback(1)",
        "Callback 1",
        "Callback _\n value:=1",
        "Call _\r\n Callback(value:=1)",
    ] {
        let source = format!(
            "Sub Callback(ByVal value As Long)\nEnd Sub\nSub Run()\n{invocation}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let symbol = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Callback")
            .unwrap()
            .id;
        let start = u64::try_from(source.find(invocation).unwrap()).unwrap();
        let end = start + u64::try_from(invocation.len()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        assert_eq!(
            application.target,
            OccurrenceTarget::Resolved { symbol },
            "{source}"
        );
        assert_eq!(application.role, OccurrenceRole::Reference);
    }
}

#[test]
fn vba_declared_array_application_binds_without_guessing_object_defaults() {
    let source = "Sub Run()\nDim values(2) As Long\nDim dynamic() As Long\nDim objectValue As Object\nDim output As Long\nReDim dynamic(2)\nvalues(0) = Len(\"x\")\noutput = values(1)\noutput = dynamic(1)\noutput = objectValue(1)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let array = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "values")
        .unwrap();
    let dynamic = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "dynamic")
        .unwrap();
    for (expression, target) in [
        ("values(1)", Some(array.id)),
        ("dynamic(1)", Some(dynamic.id)),
        ("objectValue(1)", None),
    ] {
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap_or_else(|| panic!("missing application: {expression}"));
        match target {
            Some(symbol) => {
                assert_eq!(application.target, OccurrenceTarget::Resolved { symbol });
                assert_eq!(application.role, OccurrenceRole::Reference);
                assert!(document.relations.iter().any(|edge| {
                    edge.predicate == RelationPredicate::RefersTo
                        && edge.subject == RelationEndpoint::Occurrence(application.id)
                        && edge.object == RelationEndpoint::Entity(symbol)
                }));
                assert!(!document.skipped_regions.iter().any(|gap| {
                    gap.detail == "vba-call-or-index-target-unavailable"
                        && gap.source.span().start_byte() == start
                        && (gap.source.span().end_byte() == end
                            || gap.source.span().end_byte()
                                == start + u64::try_from(expression.find('(').unwrap()).unwrap())
                }));
            }
            None => {
                assert!(matches!(
                    application.target,
                    OccurrenceTarget::Unresolved { .. }
                ));
                assert!(document.skipped_regions.iter().any(|gap| {
                    gap.detail == "vba-call-or-index-target-unavailable"
                        && gap.source.span() == application.source.span()
                }));
                assert!(document.skipped_regions.iter().any(|gap| {
                    gap.detail == "vba-call-or-index-target-unavailable"
                        && gap.source.span().start_byte() == start
                        && gap.source.span().end_byte()
                            == start + u64::try_from(expression.find('(').unwrap()).unwrap()
                }));
            }
        }
    }
    let assignment_start = u64::try_from(source.find("values(0) = Len").unwrap()).unwrap();
    let assignment_receiver = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.invoked_identifier.reference"
                && site.source.span().start_byte() == assignment_start
        })
        .unwrap();
    assert_eq!(
        assignment_receiver.target,
        OccurrenceTarget::Resolved { symbol: array.id }
    );
    let assignment_application = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == assignment_start
        })
        .unwrap();
    assert_eq!(
        assignment_application.target,
        OccurrenceTarget::Resolved { symbol: array.id }
    );
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.source.span() == assignment_receiver.source.span()
            && gap.detail == "vba-call-or-index-target-unavailable"
    }));
    let value_start = u64::try_from(source.find("Len(\"x\")").unwrap()).unwrap();
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.source.span().start_byte() == value_start
                && gap.detail == "vba-call-or-index-target-unavailable")
    );
}

#[test]
fn vba_array_writes_bind_exact_local_and_self_targets_without_calls() {
    for prefix in ["", "Let ", "Set "] {
        for receiver in ["items", "Me.items"] {
            let expression = format!("{receiver}(0)");
            let source = format!(
                "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate items(2) As Variant\nSub Run(ByVal value As Variant)\nDim items(2) As Variant\n{prefix}{expression} = value\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let kind = if receiver.starts_with("Me.") {
                EntityKind::Field
            } else {
                EntityKind::Variable
            };
            let target = document
                .entities
                .iter()
                .find(|entity| entity.kind == kind && entity.canonical_name == "items")
                .unwrap();
            let start = u64::try_from(source.find(&expression).unwrap()).unwrap();
            let end = start + u64::try_from(expression.len()).unwrap();
            let statement_end = end + u64::try_from(" = value".len()).unwrap();
            let application = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.application.reference"
                        && site.source.span().start_byte() == start
                        && matches!(site.source.span().end_byte(), at if at == end || at == statement_end)
                })
                .unwrap();
            assert_eq!(
                application.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{source}"
            );
            assert_eq!(application.role, OccurrenceRole::Reference);
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "vba-call-or-index-target-unavailable"
                        && gap.source.span().start_byte() >= start
                        && gap.source.span().end_byte() <= end),
                "{source}"
            );
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Calls
                        && edge.subject == RelationEndpoint::Occurrence(application.id))
            );
        }
    }
}

#[test]
fn vba_array_write_resolution_keeps_dynamic_and_procedure_barriers() {
    for declaration in ["Dim items As Object", "Dim items As Variant"] {
        for prefix in ["", "Let ", "Set "] {
            let source = format!(
                "Sub Run(ByVal value As Variant)\n{declaration}\n{prefix}items(0) = value\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let start = u64::try_from(source.find("items(0)").unwrap()).unwrap();
            let application = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.application.reference"
                        && site.source.span().start_byte() == start
                })
                .unwrap();
            assert!(
                matches!(application.target, OccurrenceTarget::Unresolved { .. }),
                "{source}"
            );
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == application.source
                        && gap.detail == "vba-call-or-index-target-unavailable")
            );
        }
    }
    let source = "Sub Accept(ByVal flag As Boolean)\nEnd Sub\nSub Run(ByVal value As Long)\nAccept (0) = value\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let start = u64::try_from(source.find("Accept (0)").unwrap()).unwrap();
    let application = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == start
        })
        .unwrap();
    assert!(matches!(
        application.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.source == application.source
                && gap.detail == "vba-call-or-index-target-unavailable")
    );
}

#[test]
fn vba_array_parameters_bind_indexed_reads_to_their_declarations() {
    for parameter in [
        "ByRef items() As Long",
        "items() As String",
        "ParamArray items() As Variant",
        "ByRef items%()",
        "bYrEf items( ) As Long",
        "ByRef _\nitems() As Long",
        "ByRef items _\n( _\n ) As Long",
        "ByRef items _\r\n( _\r\n ) As Long",
        "ParamArray items( _\n ) As Variant",
        "Optional ByRef items( _\n ) As Variant",
    ] {
        let source =
            format!("Sub Run({parameter})\nDim first As Variant\nfirst = items(0)\nEnd Sub\n");
        let result = output(&source);
        let document = result.document();
        let parameter = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "items")
            .unwrap();
        let start = u64::try_from(source.find("items(0)").unwrap()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 8
            })
            .unwrap();
        assert_eq!(
            application.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            },
            "{source}"
        );
        assert_eq!(application.role, OccurrenceRole::Reference);
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(application.id)
                && edge.object == RelationEndpoint::Entity(parameter.id)
        }));
        assert!(!document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(application.id)
        }));
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.source.span().start_byte() == start
                && matches!(gap.source.span().end_byte() - start, 5 | 8)
                && gap.detail == "vba-call-or-index-target-unavailable"
        }));
    }
}

#[test]
fn vba_proven_array_targets_preserve_argument_and_returned_value_gaps() {
    let source = "Sub Run(ByRef values() As Long)\nDim first As Variant\nfirst = values(Missing(0))\nfirst = values(0)(1)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let argument_start = u64::try_from(source.find("Missing(0)").unwrap()).unwrap();
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail == "vba-call-or-index-target-unavailable"
            && gap.source.span().start_byte() == argument_start
    }));
    let outer_start = u64::try_from(source.find("values(0)(1)").unwrap()).unwrap();
    let outer = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == outer_start
                && site.source.span().end_byte() == outer_start + 12
        })
        .unwrap();
    assert!(matches!(outer.target, OccurrenceTarget::Unresolved { .. }));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail == "vba-call-or-index-target-unavailable"
            && gap.source.span() == outer.source.span()
    }));
    for expression in ["values(Missing(0))", "values(0)(1)"] {
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.detail == "vba-call-or-index-target-unavailable"
                && gap.source.span().start_byte() == start
                && gap.source.span().end_byte() == start + 6
        }));
    }
}

#[test]
fn vba_array_parameter_binding_preserves_unknown_or_invalid_shapes() {
    for parameters in [
        "ByVal items() As Long",
        "ByRef items As Variant",
        "ByVal items As Object",
        "ByRef items(2) As Long",
        "ByVal items( _\n ) As Long",
        "ByRef items( _\n 2) As Long",
        "ByRef items() As Long, items As Variant",
    ] {
        let source =
            format!("Sub Run({parameters})\nDim first As Variant\nfirst = items(0)\nEnd Sub\n");
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.find("items(0)").unwrap()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        assert!(
            matches!(application.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.source.span() == application.source.span()
                && gap.detail == "vba-call-or-index-target-unavailable"
        }));
    }
}

#[test]
fn vba_conditional_array_declaration_does_not_choose_a_compile_branch() {
    let source = "Sub Run()\n#If Flag Then\nDim values(2) As Long\n#Else\nDim values As Object\n#End If\nDim output As Long\noutput = values(1)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let start = u64::try_from(source.find("values(1)").unwrap()).unwrap();
    let application = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == start
        })
        .unwrap();
    assert!(matches!(
        application.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail == "vba-call-or-index-target-unavailable"
            && gap.source.span() == application.source.span()
    }));
}

#[test]
fn vba_function_values_do_not_guess_required_arguments_or_sub_results() {
    for declaration in [
        "Function Measure(ByVal input As Long) As Long\nEnd Function",
        "Function Measure(ByVal input As Long, Optional extra As Long = 1) As Long\nEnd Function",
        "Function Measure(ByVal input As Long, ParamArray rest() As Variant) As Long\nEnd Function",
        "Sub Measure()\nEnd Sub",
    ] {
        for expression in ["mEaSuRe", "Me.mEaSuRe"] {
            let source = format!(
                "VERSION 1.0 CLASS\nAttribute VB_Name = \"Meter\"\n{declaration}\nSub Run()\nDim value As Long\nvalue = {expression}\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let start = u64::try_from(source.find("mEaSuRe").unwrap()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == start
                        && site.source.span().end_byte() == start + 7
                })
                .unwrap();
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{declaration}: {expression}"
            );
            assert!(!document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
            }));
        }
    }
}

#[test]
fn vba_function_values_keep_ambiguous_and_nonvalue_uses_unproven() {
    for (declaration, statement, marker) in [
        (
            "Function Measure() As Long\nEnd Function",
            "mEaSuRe = 1",
            "mEaSuRe",
        ),
        (
            "Function Measure() As Long\nEnd Function",
            "value = mEaSuRe$",
            "mEaSuRe$",
        ),
        (
            "#If Available Then\nFunction Measure() As Long\nEnd Function\n#End If",
            "value = mEaSuRe",
            "mEaSuRe",
        ),
        (
            "Function Measure() As Long\nEnd Function\nFunction Measure() As Long\nEnd Function",
            "value = mEaSuRe",
            "mEaSuRe",
        ),
    ] {
        let source = format!("{declaration}\nSub Run()\nDim value As Long\n{statement}\nEnd Sub\n");
        let result = output(&source);
        let site = reference_at(result.document(), &source, marker);
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert_ne!(site.role, OccurrenceRole::CallSite);
    }
}

#[test]
fn vba_function_values_preserve_result_and_local_shadowing() {
    let source = "Function Measure() As Long\nMeasure = 1\nMeasure = mEaSuRe + 1\nEnd Function\nSub Run(Optional Measure As Long = 1)\nDim value As Long\nvalue = MEASURE\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let result_read = reference_at(document, source, "mEaSuRe +");
    let function = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "Measure")
        .unwrap();
    assert_eq!(
        result_read.target,
        OccurrenceTarget::Resolved {
            symbol: function.id
        }
    );
    assert_eq!(result_read.role, OccurrenceRole::Reference);
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "Measure")
        .unwrap();
    let local_read = reference_at(document, source, "MEASURE\n");
    assert_eq!(
        local_read.target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
    assert_eq!(local_read.role, OccurrenceRole::Reference);
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.role != OccurrenceRole::CallSite)
    );
}

#[test]
fn vba_self_property_access_selects_get_let_and_set_sources() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate stored As Variant\nPublic Property Get Item() As Variant\nItem = stored\nEnd Property\nPublic Property Let Item(ByVal value As Variant)\nstored = value\nEnd Property\nPublic Property Set Item(ByVal value As Object)\nSet stored = value\nEnd Property\nPublic Sub Run(ByVal value As Object)\nDim Item As Variant\nItem = Me.iTeM\nLet Me.[Item] = 4\nSet Me.ITEM = value\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    for (declaration, access, spelling) in [
        ("Get Item", "Me.iTeM", "iTeM"),
        ("Let Item", "Me.[Item]", "[Item]"),
        ("Set Item", "Me.ITEM", "ITEM"),
    ] {
        let definition_start = u64::try_from(source.find(declaration).unwrap() + 4).unwrap();
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == definition_start
            })
            .unwrap();
        let start = u64::try_from(source.find(access).unwrap() + 3).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte()
                        == start + u64::try_from(spelling.len()).unwrap()
            })
            .unwrap();
        assert_eq!(site.target, definition.target, "{access}");
        assert_eq!(site.role, OccurrenceRole::CallSite, "{access}");
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("accessor must be source-bound")
        };
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(symbol)
        }));
    }
}

#[test]
fn vba_self_call_forms_resolve_only_the_declared_callee() {
    for invocation in [
        "Me.Visit 1",
        "Call Me.Visit(1)",
        "Me.Visit(1)",
        "Call Me.[Visit](1)",
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Counter\"\nPrivate Sub Visit(ByVal value As Long)\nEnd Sub\nPublic Sub Run()\n{invocation}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Visit")
            .unwrap();
        let start = u64::try_from(source.rfind("Me.").unwrap() + 3).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{invocation}"
        );
        assert_eq!(site.role, OccurrenceRole::CallSite, "{invocation}");
        assert_eq!(
            site.source.span().end_byte() - start,
            if invocation.contains('[') { 7 } else { 5 }
        );
    }
}

#[test]
fn vba_self_properties_preserve_read_and_indexed_assignment_contexts() {
    let declarations = "Public Property Get Item(ByVal index As Long) As Variant\nItem = index\nEnd Property\nPublic Property Let Item(ByVal index As Long, ByVal value As Variant)\nEnd Property\nPublic Property Set Item(ByVal index As Long, ByVal value As Object)\nEnd Property\n";
    for (statement, accessor) in [
        ("value = Me.Item(0)", "Get Item"),
        ("Let Me.Item(0) = value", "Let Item"),
        ("Me.Item(0) = value", "Let Item"),
        ("Set Me.Item(0) = value", "Set Item"),
        ("If Me.Item(0) Then\nEnd If", "Get Item"),
        ("value = 1 + Me.Item(0)", "Get Item"),
        ("value = (Me.Item(0))", "Get Item"),
        ("Visit Me.Item(0)", "Get Item"),
        ("Set value = Me.Item(0)", "Get Item"),
        ("Me.Item(0).Name = value", "Get Item"),
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\n{declarations}Sub Run(ByVal value As Variant)\n{statement}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let definition_start = u64::try_from(source.find(accessor).unwrap() + 4).unwrap();
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == definition_start
            })
            .unwrap();
        let start = u64::try_from(source.rfind("Me.Item").unwrap() + 3).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 4
            })
            .unwrap();
        assert_eq!(site.target, definition.target, "{statement}");
        assert_eq!(site.role, OccurrenceRole::CallSite, "{statement}");
    }
}

#[test]
fn vba_self_value_calls_and_indexed_fields_keep_distinct_roles() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate items(2) As Variant\nFunction Measure(ByVal index As Long) As Long\nMeasure = index\nEnd Function\nSub Run(ByVal value As Variant)\nvalue = Me.Measure(1)\nLet Me.items(0) = value\nSet Me.items(1) = value\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    for (marker, role) in [
        ("Measure(1)", OccurrenceRole::CallSite),
        ("items(0)", OccurrenceRole::Reference),
        ("items(1)", OccurrenceRole::Reference),
    ] {
        let start = u64::try_from(source.rfind(marker).unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            "{marker}"
        );
        assert_eq!(site.role, role, "{marker}");
        if role == OccurrenceRole::Reference {
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source.span().start_byte() == start
                        && gap.detail == "vba-call-or-index-target-unavailable")
            );
        }
    }
}

#[test]
fn vba_self_properties_do_not_guess_missing_or_ambiguous_accessors() {
    let get = "Public Property Get Item() As Variant\nItem = 1\nEnd Property\n";
    let set = "Public Property Set Item(ByVal value As Object)\nEnd Property\n";
    for (class, declarations, statement) in [
        (false, get.to_owned(), "value = Me.Item"),
        (true, get.to_owned(), "Me.Item = 2"),
        (true, get.to_owned(), "Set Me.Item = value"),
        (true, set.to_owned(), "value = Me.Item"),
        (true, get.repeat(2), "value = Me.Item"),
        (
            true,
            format!("{get}Private Item As Variant\n"),
            "value = Me.Item",
        ),
        (
            true,
            format!("Private Item As Variant\n{get}"),
            "value = Me.Item",
        ),
        (
            true,
            format!("#If Enabled Then\n{get}#End If\n"),
            "value = Me.Item",
        ),
        (true, get.to_owned(), "ReDim Me.Item(3)"),
        (true, get.to_owned(), "value = Me.Item$"),
        (true, get.to_owned(), "Call Me.Item()"),
    ] {
        let prefix = if class { "VERSION 1.0 CLASS\n" } else { "" };
        let source = format!(
            "{prefix}Attribute VB_Name = \"Holder\"\n{declarations}Sub Run(ByVal value As Variant)\n{statement}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.rfind("Me.Item").unwrap() + 3).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() == start
                    && gap.domain == FactDomain::Relations),
            "{source}"
        );
    }
}

#[test]
fn vba_self_members_keep_unproven_owners_and_targets_explicit() {
    for (prefix, declarations, access) in [
        ("", "Private total As Long\n", "Me.total"),
        (
            "VERSION 1.0 CLASS\n",
            "Private total As Long\n",
            "other.total",
        ),
        ("VERSION 1.0 CLASS\n", "Private total As Long\n", "Me!total"),
        (
            "VERSION 1.0 CLASS\n",
            "Private total As Long\n",
            "Me.total$",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private total As Long\nPrivate TOTAL As Long\n",
            "Me.total",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "#If Wide Then\nPrivate total As Long\n#End If\n",
            "Me.total",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private Const total = 1\n",
            "Me.total",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private Declare PtrSafe Function total Lib \"clock\" () As LongPtr\n",
            "Me.total()",
        ),
    ] {
        let source = format!(
            "{prefix}Attribute VB_Name = \"Counter\"\n{declarations}Public Sub Run()\nDim value As Variant\nvalue = {access}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.rfind(access).unwrap()).unwrap();
        let end = start + u64::try_from(access.len()).unwrap();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() >= start
                    && site.source.span().end_byte() <= end
                    && site.syntax_kind.as_str() != "vba.instance.reference"
            })
            .collect();
        assert!(!sites.is_empty(), "{source}");
        assert!(
            sites
                .iter()
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. })),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() >= start
                    && gap.source.span().end_byte() <= end
                    && matches!(
                        gap.detail.as_str(),
                        "vba-member-target-unavailable" | "vba-call-or-index-target-unavailable"
                    )),
            "{source}"
        );
    }
}

#[test]
fn vba_self_indexed_fields_bind_array_applications_without_method_calls() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Counter\"\nPrivate items(1) As Long\nPublic Sub Run()\nDim value As Long\nvalue = Me.items(0)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let start = u64::try_from(source.rfind("items(0)").unwrap()).unwrap();
    let field = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "items")
        .unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| site.source.span().start_byte() == start)
        .unwrap();
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: field.id });
    assert_eq!(site.role, OccurrenceRole::Reference);
    let application = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == start - 3
                && site.source.span().end_byte() == start + 8
        })
        .unwrap();
    assert_eq!(
        application.target,
        OccurrenceTarget::Resolved { symbol: field.id }
    );
    assert_eq!(application.role, OccurrenceRole::Reference);
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Calls
            && matches!(edge.subject, RelationEndpoint::Occurrence(id) if id == site.id || id == application.id)
    }));
    assert!(document.skipped_regions.iter().all(
        |gap| !(matches!(gap.source.span().start_byte(), at if at == start || at == start - 3)
            && gap.detail == "vba-call-or-index-target-unavailable")
    ));
}

#[test]
fn vba_self_array_reads_preserve_spelling_and_bypass_local_shadows() {
    for expression in [
        "Me.items(0)",
        "mE . [items](0)",
        "Me. _\nitems(0)",
        "Me.items(Me.items(0))",
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Counter\"\nPrivate items(1) As Long\nPublic Sub Run(ByVal items As Object)\nDim value As Long\nvalue = {expression}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let field = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "items")
            .unwrap();
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        assert_eq!(
            application.target,
            OccurrenceTarget::Resolved { symbol: field.id },
            "{source}"
        );
        assert_eq!(application.role, OccurrenceRole::Reference);
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-call-or-index-target-unavailable"
                    && gap.source.span().start_byte() >= start
                    && gap.source.span().end_byte() <= end),
            "{source}"
        );
    }
}

#[test]
fn vba_self_array_reads_require_class_array_and_direct_application_evidence() {
    for (header, declarations, expression) in [
        ("", "Private items(1) As Long", "Me.items(0)"),
        (
            "VERSION 1.0 CLASS\n",
            "Private items As Object",
            "Me.items(0)",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private items(1) As Long\nPrivate items(2) As Long",
            "Me.items(0)",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private items(1) As Long",
            "[Me].items(0)",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private items(1) As Long",
            "Me.items(0)(1)",
        ),
    ] {
        let source = format!(
            "{header}Attribute VB_Name = \"Counter\"\n{declarations}\nPublic Sub Run()\nDim value As Long\nvalue = {expression}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        assert!(
            matches!(application.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-call-or-index-target-unavailable"
                    && gap.source == application.source),
            "{source}"
        );
    }
}

#[test]
fn vba_self_members_bypass_locals_and_preserve_exact_name_sources() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Counter\"\nPrivate total As Long\nPrivate Sub Refresh()\nEnd Sub\nPublic Sub Run()\nDim total As Long\nDim Refresh As Long\ntotal = 1\nMe.total = total\nCall mE.ReFrEsH()\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let class = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class)
        .unwrap();
    for (marker, name, kind, role) in [
        (
            "total = total",
            "total",
            EntityKind::Field,
            OccurrenceRole::Reference,
        ),
        (
            "ReFrEsH()",
            "Refresh",
            EntityKind::Method,
            OccurrenceRole::CallSite,
        ),
    ] {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap();
        assert_eq!(target.container, Some(ContainerRef::Entity(class.id)));
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + u64::try_from(name.len()).unwrap()
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(site.role, role);
    }
}

#[test]
fn vba_native_class_headers_preserve_members_sources_and_body_identity() {
    let source = "Attribute VB_Name = \"Counter\"\nAttribute VB_GlobalNameSpace = False\nAttribute VB_Creatable = False\nAttribute VB_PredeclaredId = False\nAttribute VB_Exposed = False\nPrivate total As Long\nPublic Sub Refresh()\ntotal = 1\nEnd Sub\nPublic Sub Run()\nMe.Refresh\nEnd Sub\n";
    let kind = Some(rootlight_adapter_sdk::NativeVbaModuleKind::Object);
    let first = output_with_native_kind(source, kind);
    let second_source = source.replace("total = 1", "total = 2 + 3");
    let second = output_with_native_kind(&second_source, kind);
    for (result, bytes) in [
        (&first, source.as_bytes()),
        (&second, second_source.as_bytes()),
    ] {
        let document = result.document();
        let owner = document
            .entities
            .iter()
            .find(|e| e.canonical_name == "Counter")
            .unwrap();
        assert_eq!(owner.kind, EntityKind::Class);
        for (name, kind) in [
            ("Counter", EntityKind::Class),
            ("total", EntityKind::Field),
            ("Refresh", EntityKind::Method),
            ("Run", EntityKind::Method),
        ] {
            let member = document
                .entities
                .iter()
                .find(|e| e.canonical_name == name)
                .unwrap();
            assert_eq!(member.kind, kind);
            assert_eq!(
                member.id,
                first
                    .document()
                    .entities
                    .iter()
                    .find(|e| e.canonical_name == name)
                    .unwrap()
                    .id
            );
            if name != "Counter" {
                assert_eq!(member.container, Some(ContainerRef::Entity(owner.id)));
            }
            let definition = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: member.id }
                })
                .unwrap();
            let span = definition.source.span();
            let text = &bytes[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert_eq!(std::str::from_utf8(text).unwrap().trim_matches('"'), name);
        }
        let refresh = document
            .entities
            .iter()
            .find(|e| e.canonical_name == "Refresh")
            .unwrap();
        assert!(document.occurrences.iter().any(|site| site.target
            == OccurrenceTarget::Resolved { symbol: refresh.id }
            && site.role == OccurrenceRole::CallSite));
    }
}

#[test]
fn vba_native_module_category_requires_explicit_project_context() {
    use rootlight_adapter_sdk::NativeVbaModuleKind;
    for attributes in [
        "",
        "Attribute VB_Exposed = False\nAttribute VB_PredeclaredId = True\n",
    ] {
        let source = format!(
            "Attribute VB_Name = \"Worker\"\n{attributes}Public value As Long\nPublic Sub Run()\nEnd Sub\n"
        );
        for (context, expected) in [
            (None, EntityKind::Module),
            (Some(NativeVbaModuleKind::Procedural), EntityKind::Module),
            (Some(NativeVbaModuleKind::Object), EntityKind::Class),
        ] {
            let result = output_with_native_kind(&source, context);
            let entity = result
                .document()
                .entities
                .iter()
                .find(|e| e.canonical_name == "Worker")
                .unwrap();
            assert_eq!(entity.kind, expected);
        }
    }
}

#[test]
fn vba_class_exports_preserve_typed_members_and_body_independent_identity() {
    let source = "VERSION 1.0 CLASS\nBEGIN\n  MultiUse = -1\nEND\nAttribute VB_Name = \"Counter\"\nOption Explicit\nPrivate total As Long\nPrivate Declare PtrSafe Function ReadClock Lib \"clock\" () As LongPtr\nPublic Event Changed(ByVal value As Long)\nPublic Property Get Current() As Long\nCurrent = total\nEnd Property\nPublic Sub Refresh()\nDim localValue As Long\nlocalValue = 1\ntotal = localValue\nEnd Sub\nPublic Sub Run()\nRefresh\nEnd Sub\n";
    let first = output(source);
    let second = output(&source.replace("localValue = 1", "localValue = 2 + 3"));
    for result in [&first, &second] {
        let document = result.document();
        let class = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Counter")
            .unwrap();
        assert_eq!(class.kind, EntityKind::Class);
        for (name, kind) in [
            ("total", EntityKind::Field),
            ("ReadClock", EntityKind::Function),
            ("Changed", EntityKind::Event),
            ("Current", EntityKind::Property),
            ("Refresh", EntityKind::Method),
            ("Run", EntityKind::Method),
        ] {
            let member = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap();
            assert_eq!(member.kind, kind, "{name}");
            assert_eq!(
                member.container,
                Some(ContainerRef::Entity(class.id)),
                "{name}"
            );
            let original = first
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap();
            assert_eq!(member.id, original.id, "{name}");
        }
        assert_eq!(
            class.id,
            first
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "Counter")
                .unwrap()
                .id
        );
        let refresh = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Refresh")
            .unwrap();
        let local = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "localValue")
            .unwrap();
        assert_eq!(local.kind, EntityKind::Variable);
        assert_eq!(local.container, Some(ContainerRef::Entity(refresh.id)));
        assert!(
            document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite
                    && site.target == OccurrenceTarget::Resolved { symbol: refresh.id })
        );
        let definition = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.module_name.definition")
            .unwrap();
        assert_eq!(
            definition.target,
            OccurrenceTarget::Resolved { symbol: class.id }
        );
        assert_eq!(
            definition.source.span().start_byte(),
            u64::try_from(source.find("\"Counter\"").unwrap()).unwrap()
        );
    }
}

#[test]
fn vba_exported_module_names_preserve_authored_identity_and_source() {
    for (prefix, kind) in [
        ("", EntityKind::Module),
        (
            "VERSION 1.0 CLASS\r\nBEGIN\r\n  MultiUse = -1\r\nEND\r\n",
            EntityKind::Class,
        ),
    ] {
        let source = format!(
            "{prefix}Attribute VB_Name = \"Metrics\"\r\nOption Explicit\r\nSub Run()\r\nEnd Sub\r\n"
        );
        let result = output(&source);
        let document = result.document();
        let module = document
            .entities
            .iter()
            .find(|entity| entity.kind == kind)
            .unwrap();
        assert_eq!(module.canonical_name, "Metrics");
        let definition = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.module_name.definition")
            .unwrap();
        assert_eq!(
            definition.target,
            OccurrenceTarget::Resolved { symbol: module.id }
        );
        let start = source.find("\"Metrics\"").unwrap();
        assert_eq!(
            definition.source.span().start_byte(),
            u64::try_from(start).unwrap()
        );
        assert_eq!(
            definition.source.span().end_byte(),
            u64::try_from(start + "\"Metrics\"".len()).unwrap()
        );
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "Run")
        );
    }
}

#[test]
fn vba_procedural_module_qualifier_resolves_own_member_without_hiding_a_local() {
    for shadow in [
        "",
        "Dim Utility As Variant\n",
        "#If FEATURE Then\nDim Utility As Variant\n#End If\n",
    ] {
        let source = format!(
            "Attribute VB_Name = \"Utility\"\nPublic Function Measure(ByVal value As Long) As Long\nMeasure = value\nEnd Function\nPublic Sub Run()\n{shadow}Dim result As Long\nresult = Utility.Measure(1)\nEnd Sub\n"
        );
        let document = output(&source);
        let document = document.document();
        let module = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Utility")
            .unwrap();
        let measure = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Function && entity.canonical_name == "Measure"
            })
            .unwrap();
        let start = u64::try_from(source.find("Utility.Measure(1)").unwrap()).unwrap();
        let qualifier = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 7
                    && site.syntax_kind == "vba.value_identifier.reference"
            })
            .unwrap();
        let member = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 15
                    && site.syntax_kind == "vba.invoked_member.reference"
            })
            .unwrap();
        if shadow.is_empty() {
            assert_eq!(
                qualifier.target,
                OccurrenceTarget::Resolved { symbol: module.id }
            );
            assert_eq!(
                member.target,
                OccurrenceTarget::Resolved { symbol: measure.id }
            );
            assert_eq!(member.role, OccurrenceRole::CallSite);
            assert!(document.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::Calls
                    && relation.subject == RelationEndpoint::Occurrence(member.id)
                    && relation.object == RelationEndpoint::Entity(measure.id)
            }));
            assert!(!document.skipped_regions.iter().any(|gap| {
                gap.source == qualifier.source && gap.detail == "vba-binding-target-unavailable"
                    || gap.source == member.source && gap.detail == "vba-member-target-unavailable"
            }));
        } else {
            assert_ne!(
                qualifier.target,
                OccurrenceTarget::Resolved { symbol: module.id }
            );
            assert_ne!(
                member.target,
                OccurrenceTarget::Resolved { symbol: measure.id }
            );
        }
    }
}

#[test]
fn vba_procedural_module_qualifier_requires_an_unambiguous_header_and_member() {
    for (prefix, target) in [
        ("Attribute VB_Name = \"Invalid.Name\"\n", "Measure"),
        (
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Utility\"\n",
            "Measure",
        ),
        ("Attribute VB_Name = \"Utility\"\n", "Missing"),
    ] {
        let source = format!(
            "{prefix}Public Function Measure(ByVal value As Long) As Long\nMeasure = value\nEnd Function\nPublic Sub Run()\nDim result As Long\nresult = Utility.{target}(1)\nresult = Utility\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.find(&format!("Utility.{target}(1)")).unwrap()).unwrap();
        let member = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 15
                    && site.syntax_kind == "vba.invoked_member.reference"
            })
            .unwrap();
        assert!(matches!(member.target, OccurrenceTarget::Unresolved { .. }));
        let bare =
            u64::try_from(source.find("result = Utility\n").unwrap() + "result = ".len()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == bare
                    && site.source.span().end_byte() == bare + 7
                    && site.syntax_kind == "vba.value_identifier.reference"
            })
            .unwrap();
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    }
}

#[test]
fn vba_member_paths_preserve_logical_continuations() {
    for separator in [".", " _\n.", ". _\r\n ", " _\r\n. _\n "] {
        let call = format!("Utility{separator}[Measure]");
        let field = format!("item{separator}nested{separator}[Value]");
        let constant = format!("Phase{separator}[Ready]");
        let source = format!(
            "Attribute VB_Name = \"Utility\"\nPrivate Type Inner\nValue As Long\nEnd Type\nPrivate Type Outer\nnested As Inner\nEnd Type\nPrivate Enum Phase\nReady = 1\nEnd Enum\nFunction Measure() As Long\nMeasure = 1\nEnd Function\nSub Run()\nDim item As Outer\nDim result As Long\nresult = {call}()\nresult = {field}\nresult = {constant}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        for (text, name, role) in [
            ("Utility", "Utility", OccurrenceRole::Reference),
            (call.as_str(), "Measure", OccurrenceRole::CallSite),
            (field.as_str(), "Value", OccurrenceRole::Reference),
            (constant.as_str(), "Ready", OccurrenceRole::Reference),
        ] {
            let start = u64::try_from(source.rfind(text).unwrap()).unwrap();
            let end = start + u64::try_from(text.len()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == start
                        && site.source.span().end_byte() == end
                        && matches!(
                            site.syntax_kind.as_str(),
                            "vba.member.reference"
                                | "vba.invoked_member.reference"
                                | "vba.value_identifier.reference"
                        )
                })
                .expect("complete authored member path");
            let entity = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap();
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved { symbol: entity.id },
                "{text:?}"
            );
            assert_eq!(site.role, role, "{text:?}");
            assert!(
                !document.skipped_regions.iter().any(|gap| {
                    gap.source == site.source && gap.detail == "vba-member-target-unavailable"
                }),
                "{text:?}"
            );
        }
    }
}

#[test]
fn vba_continued_self_field_paths_bypass_local_receivers() {
    let text = "Me _\n.item. _\r\n [Value]";
    let source = format!(
        "VERSION 1.0 CLASS\nAttribute VB_Name = \"Reader\"\nPrivate Type Data\nValue As Long\nEnd Type\nPrivate item As Data\nSub Run(ByVal item As Object)\nDim result As Long\nresult = {text}\nEnd Sub\n"
    );
    let result = output(&source);
    let document = result.document();
    let start = u64::try_from(source.find(text).unwrap()).unwrap();
    let end = start + u64::try_from(text.len()).unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| {
            site.source.span().start_byte() == start && site.source.span().end_byte() == end
        })
        .unwrap();
    let field = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Value")
        .unwrap();
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: field.id });
    assert_eq!(site.role, OccurrenceRole::Reference);
}

#[test]
fn vba_continued_member_paths_preserve_shadowing_and_unknown_targets() {
    for (prefix, local, path) in [
        ("", "Dim Utility As Object\n", "Utility _\n.Measure"),
        ("", "", "Unknown _\n.Measure"),
        ("", "", "Utility. _\n Missing"),
        ("", "Dim item As Object\n", "item _\n.Value"),
        ("", "Dim item As Data\n", "item _\n.Missing"),
        ("#If Enabled Then\n", "", "Utility _\n.Measure"),
    ] {
        let conditional_end = if prefix.is_empty() { "" } else { "#End If\n" };
        let source = format!(
            "Attribute VB_Name = \"Utility\"\nPrivate Type Data\nValue As Long\nEnd Type\n{prefix}Function Measure() As Long\nEnd Function\n{conditional_end}Sub Run()\n{local}Dim result As Variant\nresult = {path}(0)\nEnd Sub\n"
        );
        let result = output(&source);
        let start = u64::try_from(source.find(path).unwrap()).unwrap();
        let members: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == start
                    && matches!(
                        site.syntax_kind.as_str(),
                        "vba.member.reference" | "vba.invoked_member.reference"
                    )
            })
            .collect();
        assert!(!members.is_empty(), "{path}");
        assert!(
            members
                .iter()
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. })),
            "{path}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() == start),
            "{path}"
        );
    }
}

#[test]
fn vba_exported_module_header_uses_last_attribute_not_body_attributes() {
    let source = "Attribute VB_Name = \"OldName\"\nAttribute vb_name = \"CurrentName\"\nSub Run()\nAttribute Run.VB_Name = \"NotModule\"\nEnd Sub\nAttribute VB_Name = \"AfterBody\"\n";
    let result = output(source);
    let document = result.document();
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module)
        .unwrap();
    assert_eq!(module.canonical_name, "CurrentName");
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "vba.module_name.definition")
            .count(),
        1
    );
}

#[test]
fn vba_unproven_module_names_keep_file_identity_and_scoped_gaps() {
    for name in [
        "\"Invalid.Name\"",
        "\"Name With Space\"",
        "\"9Name\"",
        "\"Δelta\"",
        "\"abcdefghijklmnopqrstuvwxyzABCDEF\"",
        "42",
    ] {
        let source = format!(
            "Attribute VB_Name = \"Earlier\"\nAttribute VB_Name = {name}\nSub Run()\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let module = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Module)
            .unwrap();
        assert_eq!(module.canonical_name, VBA.path, "{name}");
        assert!(
            document
                .occurrences
                .iter()
                .all(|site| site.syntax_kind != "vba.module_name.definition")
        );
        let start = u64::try_from(source.find(&format!("= {name}")).unwrap() + 2).unwrap();
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-module-name-unavailable"
                    && gap.source.span().start_byte() == start),
            "{name}: {:?}",
            document.skipped_regions
        );
    }
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
        matches!(
            site.syntax_kind.as_str(),
            "vba.identifier.reference" | "vba.let_identifier.reference"
        ) && source.get(
            usize::try_from(site.source.span().start_byte()).unwrap()
                ..usize::try_from(site.source.span().end_byte()).unwrap(),
        ) == Some("Reduce")
    }) {
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
        assert_eq!(site.role, OccurrenceRole::Reference);
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
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() == start
                    && gap.domain == FactDomain::Relations),
            marker == "VISIT(1)"
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
        .filter(|fact| fact.syntax_kind().as_str() == "vba.function.signature")
        .map(|fact| {
            &VBA.source[usize::try_from(fact.span().start_byte()).unwrap()
                ..usize::try_from(fact.span().end_byte()).unwrap()]
        })
        .collect();
    assert_eq!(
        signatures,
        ["Public Function Measure(ByVal value As Long) As Long"]
    );
    let visibility = parsed
        .facts()
        .iter()
        .find(|fact| fact.syntax_kind().as_str() == "vba.public.expression")
        .unwrap();
    assert_eq!(
        &VBA.source[usize::try_from(visibility.span().start_byte()).unwrap()
            ..usize::try_from(visibility.span().end_byte()).unwrap()],
        signatures[0]
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
            matches!(
                site.syntax_kind.as_str(),
                "vba.identifier.reference"
                    | "vba.value_identifier.reference"
                    | "vba.let_identifier.reference"
                    | "vba.set_identifier.reference"
            ) && site.source.span().start_byte() == start
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
fn vba_raise_event_binds_its_class_declaration_not_a_local_or_runtime_handler() {
    use rootlight_ir::{RelationEndpoint, RelationPredicate};
    for header in ["VERSION 1.0 CLASS\n", ""] {
        let source = format!(
            "{header}Attribute VB_Name = \"Emitter\"\nPublic Event Changed(ByVal value As Long)\nPublic Sub Send(ByVal changed As Long)\nRaiseEvent CHANGED(changed)\nEnd Sub\n"
        );
        let result = output_with_native_kind(
            &source,
            header
                .is_empty()
                .then_some(rootlight_adapter_sdk::NativeVbaModuleKind::Object),
        );
        let document = result.document();
        let event = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Event && entity.canonical_name == "Changed")
            .unwrap();
        let start = u64::try_from(source.find("CHANGED(changed)").unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: event.id });
        assert_ne!(site.role, OccurrenceRole::CallSite);
        assert_eq!(site.syntax_kind, "vba.event_identifier.reference");
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.object == RelationEndpoint::Entity(event.id)
                    && edge.evidence.source.as_ref() == Some(&site.source))
        );
        assert!(!document.relations.iter().any(|edge| matches!(
            edge.predicate,
            RelationPredicate::Calls | RelationPredicate::DispatchCandidate
        ) && edge.evidence.source.as_ref()
            == Some(&site.source)));
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span() == site.source.span()
                    && gap.detail == "vba-event-handlers-unavailable")
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span() == site.source.span()
                    && gap.detail == "vba-event-target-unavailable")
        );
        let argument_start = u64::try_from(source.find("changed)\nEnd Sub").unwrap()).unwrap();
        let argument = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == argument_start)
            .unwrap();
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter && entity.canonical_name == "changed"
            })
            .unwrap();
        assert_eq!(
            argument.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            }
        );
    }
}

#[test]
fn vba_raise_event_rejects_missing_ambiguous_conditional_or_nonclass_targets() {
    for (header, declarations) in [
        ("", "Public Event Changed()\n"),
        ("VERSION 1.0 CLASS\n", ""),
        ("VERSION 1.0 CLASS\n", "Private Changed As Long\n"),
        (
            "VERSION 1.0 CLASS\n",
            "Public Event Changed()\nPublic Event changed()\n",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "#If Enabled Then\nPublic Event Changed()\n#End If\n",
        ),
    ] {
        let source = format!(
            "{header}Attribute VB_Name = \"Emitter\"\n{declarations}Public Sub Send()\nRaiseEvent Changed\nEnd Sub\n"
        );
        let result = output(&source);
        let start = u64::try_from(source.rfind("Changed\n").unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span() == site.source.span()
                    && gap.detail == "vba-event-target-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn vba_events_are_not_ordinary_values_or_callable_procedures() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Emitter\"\nPublic Event Changed()\nPublic Sub Send()\nRaiseEvent Changed\nCall Changed()\nDim item As Variant\nitem = Changed\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let event = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Event)
        .unwrap();
    let raises: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "vba.event_identifier.reference")
        .collect();
    assert_eq!(raises.len(), 1);
    assert_eq!(
        raises[0].target,
        OccurrenceTarget::Resolved { symbol: event.id }
    );
    for marker in ["Changed()\nDim", "Changed\nEnd"] {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.source.span().start_byte() == start)
            .collect();
        assert!(!sites.is_empty());
        assert!(
            sites
                .iter()
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
    }
}

#[test]
fn vba_event_names_are_independent_of_value_and_procedure_names() {
    for (declaration, use_site, marker) in [
        ("Private Status As Long\n", "Status = 1", "Status = 1"),
        (
            "Private Const Status As Long = 1\n",
            "item = Status",
            "Status\nEnd",
        ),
        (
            "Private Enum Phase\nStatus = 1\nEnd Enum\n",
            "item = Status",
            "Status\nEnd",
        ),
        (
            "Public Sub Status()\nEnd Sub\n",
            "Call Status()",
            "Status()\nEnd",
        ),
        (
            "Public Function Status() As Long\nStatus = 1\nEnd Function\n",
            "item = Status()",
            "Status()\nEnd",
        ),
        (
            "Public Property Get Status() As Long\nStatus = 1\nEnd Property\n",
            "item = Status",
            "Status\nEnd",
        ),
    ] {
        for event_first in [false, true] {
            let event = "Public Event Status()\n";
            let declarations = if event_first {
                format!("{event}{declaration}")
            } else {
                format!("{declaration}{event}")
            };
            let source = format!(
                "VERSION 1.0 CLASS\nAttribute VB_Name = \"Emitter\"\n{declarations}Public Sub Send()\nDim item As Long\nRaiseEvent STATUS\n{use_site}\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let event = document
                .entities
                .iter()
                .find(|entity| entity.kind == EntityKind::Event)
                .unwrap();
            let ordinary = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "Status" && entity.kind != EntityKind::Event
                })
                .unwrap();
            let raised = document
                .occurrences
                .iter()
                .find(|site| site.syntax_kind == "vba.event_identifier.reference")
                .unwrap();
            assert_eq!(
                raised.target,
                OccurrenceTarget::Resolved { symbol: event.id },
                "{source}"
            );
            let start = u64::try_from(source.rfind(marker).unwrap()).unwrap();
            let ordinary_use = document
                .occurrences
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == start
                        && site.source.span().end_byte() == start + 6
                })
                .unwrap();
            assert_eq!(
                ordinary_use.target,
                OccurrenceTarget::Resolved {
                    symbol: ordinary.id
                },
                "{source}"
            );
            assert_ne!(raised.target, ordinary_use.target);
        }
    }
}

#[test]
fn vba_event_and_value_barriers_stay_in_their_own_namespaces() {
    for (declarations, event_resolves, value_resolves) in [
        (
            "#If Enabled Then\nPublic Event Status()\n#End If\nPrivate Status As Long\n",
            false,
            true,
        ),
        (
            "Public Event Status()\n#If Enabled Then\nPrivate Status As Long\n#End If\n",
            true,
            false,
        ),
        (
            "Public Event Status()\nPublic Event status()\nPrivate Status As Long\n",
            false,
            true,
        ),
        (
            "Public Event Status()\nPrivate Status As Long\nPrivate status As Long\n",
            true,
            false,
        ),
        (
            "Public Event Status()\nPublic Event État()\nPrivate Status As Long\n",
            false,
            true,
        ),
        (
            "Public Event Status()\nPrivate Status As Long\nPrivate État As Long\n",
            true,
            false,
        ),
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Emitter\"\n{declarations}Public Sub Send()\nRaiseEvent STATUS\nStatus = 1\nEnd Sub\n"
        );
        let result = output(&source);
        for (marker, expected) in [("STATUS\n", event_resolves), ("Status = 1", value_resolves)] {
            let start = u64::try_from(source.rfind(marker).unwrap()).unwrap();
            let site = result
                .document()
                .occurrences
                .iter()
                .find(|site| site.source.span().start_byte() == start)
                .unwrap();
            assert_eq!(
                matches!(site.target, OccurrenceTarget::Resolved { .. }),
                expected,
                "{source}: {marker}"
            );
        }
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
fn vba_conditional_local_values_resolve_within_their_branch() {
    let source = "Public Sub Update()\n#If Enabled Then\nDim payload As Long\npayload = 1\n#End If\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let local = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "payload")
        .unwrap();
    let site = reference_at(document, source, "payload = 1");
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: local.id });
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.source.span() == site.source.span() && gap.detail == "vba-binding-target-unavailable"
    }));
}

#[test]
fn vba_conditional_sibling_branches_keep_distinct_local_bindings() {
    let source = "Public Sub Update()\n#If Enabled Then\nDim payload As Long\npayload = 1\n#Else\nDim payload As String\npayload = \"text\"\n#End If\npayload = 2\nEnd Sub\n";
    let result = output(source);
    let first = reference_at(result.document(), source, "payload = 1");
    let second = reference_at(result.document(), source, "payload = \"text\"");
    let outside = reference_at(result.document(), source, "payload = 2");
    let (
        OccurrenceTarget::Resolved {
            symbol: first_symbol,
        },
        OccurrenceTarget::Resolved {
            symbol: second_symbol,
        },
    ) = (&first.target, &second.target)
    else {
        panic!("each conditional branch must resolve its own local variable");
    };
    assert_ne!(first_symbol, second_symbol);
    assert!(matches!(
        outside.target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_conditional_local_collision_with_unconditional_local_stays_unresolved() {
    let source = "Public Sub Update()\nDim payload As Long\n#If Enabled Then\nDim payload As Long\npayload = 1\n#End If\nEnd Sub\n";
    let result = output(source);
    assert!(matches!(
        reference_at(result.document(), source, "payload = 1").target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_nested_conditional_duplicate_locals_stay_unresolved() {
    let source = "Public Sub Update()\n#If Outer Then\nDim payload As Long\n#If Inner Then\nDim payload As Long\npayload = 1\n#End If\n#End If\nEnd Sub\n";
    let result = output(source);
    assert!(matches!(
        reference_at(result.document(), source, "payload = 1").target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_type_label_and_argument_names_do_not_bind_to_local_values() {
    let source = "Public Type Model\nValue As Long\nEnd Type\nPublic Sub Update(ByVal Model As Long, ByVal Finish As Long, ByVal size As Long)\nDim item As Model\nGoTo Finish\nFinish:\nCall Visit(size:=size)\nEnd Sub\n";
    let result = output(source);
    let model = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Struct && entity.canonical_name == "Model")
        .unwrap();
    for (marker, is_type) in [
        ("Model\nGoTo", true),
        ("Finish\nFinish:", false),
        ("size:=", false),
    ] {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        if is_type {
            assert_eq!(site.syntax_kind, "vba.direct_type_identifier.reference");
            assert_eq!(site.role, OccurrenceRole::TypeUse);
            assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: model.id });
        } else if marker == "Finish\nFinish:" {
            let label = result
                .document()
                .entities
                .iter()
                .find(|entity| entity.kind == EntityKind::StatementLabel)
                .unwrap();
            assert_eq!(site.syntax_kind, "vba.statement_label.reference");
            assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: label.id });
        } else {
            assert_eq!(site.syntax_kind, "vba.nonvalue_identifier.reference");
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        }
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
fn vba_statement_labels_bind_control_transfers_in_their_own_namespace() {
    let source = "Sub Run(ByVal Finish As Long, ByVal choice As Long)\nOn Error GoTo Recover\nGoTo Finish\nGoSub Finish\nOn choice GoTo Finish, Recover\nOn choice GoSub Finish, Recover\nFinish:\nFinish = choice\nResume Recover\nRecover:\nResume Finish\nEnd Sub\nSub Other()\nGoTo Finish\nFinish:\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    assert_eq!(document.version, rootlight_ir::NormalizedIrVersion::V1_7);
    let labels: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::StatementLabel)
        .collect();
    assert_eq!(labels.len(), 3, "{:#?}", document.entities);
    let transfers: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.syntax_kind.as_str(),
                "vba.statement_label.reference"
                    | "vba.error_label.reference"
                    | "vba.resume_label.reference"
            )
        })
        .collect();
    assert_eq!(transfers.len(), 10, "{:#?}", document.occurrences);
    for site in transfers {
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("unresolved control transfer: {site:#?}");
        };
        let label = labels.iter().find(|label| label.id == symbol).unwrap();
        let span = site.source.span();
        let text = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert!(label.canonical_name.eq_ignore_ascii_case(text));
        let owner = if span.start_byte() < u64::try_from(source.find("Sub Other").unwrap()).unwrap()
        {
            "Run"
        } else {
            "Other"
        };
        let procedure = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner)
            .unwrap();
        assert_eq!(label.container, Some(ContainerRef::Entity(procedure.id)));
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(symbol))
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Occurrence(site.id))
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == site.source)
        );
    }
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "Finish")
        .unwrap();
    assert_eq!(
        reference_at(document, source, "Finish = choice").target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
}

#[test]
fn vba_numeric_statement_labels_compare_values_and_preserve_error_policy_zero() {
    let source = "Sub Run()\nGoTo 0010\nGoSub &HA\nOn Error GoTo 10\nResume &H0A\nGoTo 0\n10: Resume 0\n0 On Error GoTo 0\n20\nGoTo 20\nEnd Sub\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_vba::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let result = output(source);
    let document = result.document();
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.contains("parse")),
        "{:#?}",
        document.skipped_regions
    );
    let labels: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::StatementLabel)
        .collect();
    assert_eq!(labels.len(), 3, "{:#?}", document.entities);
    for (marker, target) in [
        ("0010", "10"),
        ("&HA", "10"),
        ("10\nResume", "10"),
        ("&H0A", "10"),
        ("0\n10:", "0"),
        ("20\nEnd", "20"),
    ] {
        let site = label_reference_at(document, source, marker);
        let label = labels
            .iter()
            .find(|label| label.canonical_name == target)
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: label.id },
            "{marker}: {site:#?}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
    }
    for marker in ["0\n0 On", "0\n20"] {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.source.span().start_byte() == start)
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() == start)
        );
    }
}

#[test]
fn vba_unparsed_octal_labels_remain_explicit_without_fabricated_bindings() {
    let source = "Sub Run()\nGoTo &O12\n10:\nEnd Sub\n";
    let result = output(source);
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == rootlight_ir::SkippedRegionReason::ParseError)
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|site| site.role != OccurrenceRole::Definition
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn vba_statement_label_ambiguity_is_procedure_local_and_never_binds_values() {
    for body in [
        "",
        "Finish:\nfinish:\n",
        "#If DEBUG Then\nFinish:\n#End If\n",
    ] {
        let source = format!(
            "Sub Run(ByVal Finish As Long)\nGoTo Finish\n{body}End Sub\nSub Other()\nGoTo Finish\nFinish:\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let site = label_reference_at(document, &source, "Finish\n");
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-statement-label-target-unavailable"
                    && gap.source == site.source)
        );
        let other_start =
            u64::try_from(source.rfind("GoTo Finish").unwrap() + "GoTo ".len()).unwrap();
        let other = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == other_start)
            .unwrap();
        assert!(
            matches!(other.target, OccurrenceTarget::Resolved { .. }),
            "{source}"
        );
    }
    let source = "Sub Run()\nGoTo 10\n10:\n&H0A:\nEnd Sub\n";
    let result = output(source);
    assert!(matches!(
        label_reference_at(result.document(), source, "10\n").target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_statement_labels_resolve_only_within_compatible_compile_branches() {
    let source = "Sub Run()\n#If Host Then\nOn Error GoTo Finish\n#If Trace Then\nGoTo Finish\n#End If\nFinish:\n#ElseIf Alternate Then\nGoSub Finish\nFinish:\n#Else\nResume Finish\nFinish:\n#End If\nGoTo Finish\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let transfers: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.syntax_kind.as_str(),
                "vba.statement_label.reference"
                    | "vba.error_label.reference"
                    | "vba.resume_label.reference"
            )
        })
        .collect();
    assert_eq!(transfers.len(), 5);
    let mut transfers = transfers;
    transfers.sort_by_key(|site| site.source.span().start_byte());
    let symbols: Vec<_> = transfers[..4]
        .iter()
        .map(|site| {
            let OccurrenceTarget::Resolved { symbol } = site.target else {
                panic!("same-branch transfer must resolve: {site:#?}");
            };
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == site.source)
            );
            symbol
        })
        .collect();
    assert_eq!(symbols[0], symbols[1]);
    assert_ne!(symbols[0], symbols[2]);
    assert_ne!(symbols[0], symbols[3]);
    assert_ne!(symbols[2], symbols[3]);
    for ((start, _), symbol) in source
        .match_indices("Finish:")
        .zip([symbols[0], symbols[2], symbols[3]])
    {
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
            })
            .unwrap();
        assert_eq!(definition.target, OccurrenceTarget::Resolved { symbol });
    }
    assert!(matches!(
        transfers[4].target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_statement_labels_do_not_guess_independent_or_nested_compile_conditions() {
    for body in [
        "#If Enabled Then\nFinish:\n#Else\nGoTo Finish\n#End If\n",
        "#If Enabled Then\nFinish:\n#End If\n#If Enabled Then\nGoTo Finish\n#End If\n",
        "Finish:\n#If Enabled Then\nFinish:\nGoTo Finish\n#End If\n",
        "#If Enabled Then\nFinish:\n#If Trace Then\nFinish:\n#End If\nGoTo Finish\n#End If\n",
        "#If Enabled Then\nFinish:\n#If Trace Then\nFinish:\nGoTo Finish\n#End If\n#End If\n",
        "#If Enabled Then\nFinish:\nfinish:\nGoTo Finish\n#End If\n",
        "#If Enabled Then\n10:\n&H0A:\nGoTo 10\n#End If\n",
    ] {
        let source = format!("Sub Run()\n{body}End Sub\n");
        let result = output(&source);
        let document = result.document();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.statement_label.reference")
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == site.source
                    && gap.detail == "vba-statement-label-target-unavailable")
        );
    }
}

#[test]
fn vba_statement_labels_exclude_duplicates_from_incompatible_nested_branches() {
    let source = "Sub Run()\n#If Enabled Then\nFinish:\n#If Trace Then\nFinish:\n#Else\nGoTo Finish\n#End If\n#End If\nEnd Sub\n";
    let result = output(source);
    let site = label_reference_at(result.document(), source, "Finish\n#End If");
    assert!(matches!(site.target, OccurrenceTarget::Resolved { .. }));
    let start = u64::try_from(source.find("Finish:").unwrap()).unwrap();
    let definition = result
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition && site.source.span().start_byte() == start
        })
        .unwrap();
    assert_eq!(site.target, definition.target);
}

#[test]
fn vba_statement_labels_resolve_inside_conditionally_declared_procedures() {
    let source = "#If Host Then\nSub Run()\nGoTo 0010\n10:\nEnd Sub\n#Else\nSub Run()\nGoTo &HA\n10:\nEnd Sub\n#End If\n";
    let result = output(source);
    let first = label_reference_at(result.document(), source, "0010\n");
    let second = label_reference_at(result.document(), source, "&HA\n");
    assert!(matches!(first.target, OccurrenceTarget::Resolved { .. }));
    assert!(matches!(second.target, OccurrenceTarget::Resolved { .. }));
    assert_ne!(first.target, second.target);
}

fn label_reference_at<'a>(
    document: &'a rootlight_ir::NormalizedIrDocument,
    source: &str,
    marker: &str,
) -> &'a rootlight_ir::OccurrenceRecord {
    let start = u64::try_from(source.find(marker).unwrap()).unwrap();
    document
        .occurrences
        .iter()
        .find(|site| {
            site.source.span().start_byte() == start
                && matches!(
                    site.syntax_kind.as_str(),
                    "vba.statement_label.reference"
                        | "vba.error_label.reference"
                        | "vba.resume_label.reference"
                )
        })
        .unwrap_or_else(|| {
            panic!(
                "missing label reference {marker}: {:#?}",
                document.occurrences
            )
        })
}

#[test]
fn vba_statement_named_arguments_bind_without_parentheses() {
    for invocation in [
        "Visit size:=size",
        "Visit size:=Inner(value:=size), label:=size",
        "Visit Inner(value:=size), label:=size",
        "Visit (size), label:=size",
        "Visit _\n size:=size",
        "Visit _\r\n size:=size",
        "Visit size _\n :=size",
        "Me.Visit size:=size",
        "Me.Visit _\r\n size:=size",
        "Me.Visit Inner(value:=size), label:=size",
        "Call Visit _\n (size:=size)",
        "Call Me.Visit _\n (size:=size)",
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Visitor\"\nPublic Function Inner(ByVal value As Long) As Long\nInner = value\nEnd Function\nPublic Sub Visit(ByVal size As Long, Optional label As Long = 0)\nEnd Sub\nPublic Sub Run(ByVal size As Long)\n{invocation}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        for (marker, owner_name, parameter_name) in [
            ("size:=", "Visit", "size"),
            ("size _\n :=", "Visit", "size"),
            ("label:=", "Visit", "label"),
            ("value:=", "Inner", "value"),
        ] {
            let Some(start) = source.find(marker) else {
                continue;
            };
            let owner = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == owner_name)
                .unwrap();
            let parameter = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.canonical_name == parameter_name
                        && entity.container == Some(ContainerRef::Entity(owner.id))
                })
                .unwrap();
            let label = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.nonvalue_identifier.reference"
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                })
                .unwrap_or_else(|| panic!("missing named label: {invocation}: {marker}"));
            assert_eq!(
                label.target,
                OccurrenceTarget::Resolved {
                    symbol: parameter.id
                },
                "{invocation}: {marker}"
            );
            assert!(document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.object == RelationEndpoint::Entity(parameter.id)
                    && edge.evidence.source.as_ref() == Some(&label.source)
            }));
            assert!(!document.skipped_regions.iter().any(|gap| {
                gap.source == label.source && gap.detail == "vba-nonvalue-target-unavailable"
            }));
        }
        let start = u64::try_from(source.find(invocation).unwrap()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Visit")
            .unwrap();
        assert_eq!(
            application.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{invocation}"
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == application.source
                    && gap.detail == "vba-call-or-index-target-unavailable")
        );
    }
}

#[test]
fn vba_named_arguments_bind_only_to_the_called_procedure_parameters() {
    let source = "Public Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run(ByVal size As Long)\nCall Visit(size:=size)\nCall Visit(missing:=size)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let callee = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Visit")
        .unwrap();
    let callee_parameter = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.canonical_name == "size"
                && entity.container == Some(ContainerRef::Entity(callee.id))
        })
        .unwrap();
    let caller = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Run")
        .unwrap();
    let caller_parameter = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.canonical_name == "size"
                && entity.container == Some(ContainerRef::Entity(caller.id))
        })
        .unwrap();
    let label_at = |marker: &str| {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.nonvalue_identifier.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap()
    };
    let label = label_at("size:=size");
    assert_eq!(label.syntax_kind, "vba.nonvalue_identifier.reference");
    assert_eq!(
        label.target,
        OccurrenceTarget::Resolved {
            symbol: callee_parameter.id
        }
    );
    assert_eq!(label.role, OccurrenceRole::Reference);
    assert!(document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && edge.object == RelationEndpoint::Entity(callee_parameter.id)
            && edge.evidence.source.as_ref() == Some(&label.source)
    }));
    let value_start = u64::try_from(source.find("size:=size").unwrap() + "size:=".len()).unwrap();
    let value = document
        .occurrences
        .iter()
        .find(|site| site.source.span().start_byte() == value_start)
        .unwrap();
    assert_eq!(
        value.target,
        OccurrenceTarget::Resolved {
            symbol: caller_parameter.id
        }
    );
    assert_ne!(callee_parameter.id, caller_parameter.id);
    let missing = label_at("missing:=size");
    assert!(matches!(
        missing.target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_self_named_arguments_bind_the_method_parameter_not_caller_shadows() {
    for invocation in [
        "Call Me.Visit(size:=size)",
        "Me.Visit(size:=size)",
        "Call mE . [Visit](size:=size)",
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Receiver\"\nPublic Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run(ByVal size As Long, ByVal Visit As Object)\n{invocation}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let method = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Visit" && entity.kind == EntityKind::Method)
            .unwrap();
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == "size"
                    && entity.container == Some(ContainerRef::Entity(method.id))
            })
            .unwrap();
        let label_start = u64::try_from(source.find("size:=size").unwrap()).unwrap();
        let label = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.nonvalue_identifier.reference"
                    && site.source.span().start_byte() == label_start
            })
            .unwrap();
        assert_eq!(
            label.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            },
            "{source}"
        );
        assert_eq!(label.role, OccurrenceRole::Reference);
        assert_eq!(label.source.span().end_byte(), label_start + 4);
        assert!(!document.skipped_regions.iter().any(
            |gap| gap.source == label.source && gap.detail == "vba-nonvalue-target-unavailable"
        ));
        let value = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == label_start + 6)
            .unwrap();
        assert!(
            matches!(value.target, OccurrenceTarget::Resolved { symbol } if symbol != parameter.id)
        );
    }
}

#[test]
fn vba_self_named_arguments_follow_nested_calls_and_accessor_contexts() {
    let declarations = "Public Function Inner(ByVal value As Long) As Long\nInner = value\nEnd Function\nPublic Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Property Get Item(ByVal index As Long) As Variant\nItem = index\nEnd Property\nPublic Property Let Item(ByVal index As Long, ByVal value As Variant)\nEnd Property\nPublic Property Set Item(ByVal index As Long, ByVal value As Object)\nEnd Property\n";
    for (statement, marker, owner_marker, parameter_name) in [
        (
            "Call Me.Visit(size:=Me.Inner(value:=1))",
            "size:=",
            "Sub Visit",
            "size",
        ),
        (
            "Call Me.Visit(size:=Me.Inner(value:=1))",
            "value:=",
            "Function Inner",
            "value",
        ),
        ("value = Me.Item(index:=1)", "index:=", "Get Item", "index"),
        (
            "Let Me.Item(index:=1) = value",
            "index:=",
            "Let Item",
            "index",
        ),
        (
            "Set Me.Item(index:=1) = value",
            "index:=",
            "Set Item",
            "index",
        ),
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Receiver\"\n{declarations}Sub Run(ByVal value As Variant)\n{statement}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let name_offset = owner_marker.rfind(' ').unwrap() + 1;
        let definition_start =
            u64::try_from(source.find(owner_marker).unwrap() + name_offset).unwrap();
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == definition_start
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol: owner } = definition.target else {
            panic!("accessor definition must resolve")
        };
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == parameter_name
                    && entity.container == Some(ContainerRef::Entity(owner))
            })
            .unwrap();
        let label_start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let label = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.nonvalue_identifier.reference"
                    && site.source.span().start_byte() == label_start
            })
            .unwrap();
        assert_eq!(
            label.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            },
            "{statement}: {marker}"
        );
    }
}

#[test]
fn vba_self_named_arguments_preserve_unknown_and_ambiguous_receivers() {
    let method = "Public Sub Visit(ByVal size As Long)\nEnd Sub\n";
    for (class_header, declarations, invocation, marker) in [
        (
            "VERSION 1.0 CLASS\n",
            "Public Property Let Item(ByVal value As Long, ByVal value As Long)\nEnd Property\n"
                .to_owned(),
            "Let Me.Item(value:=1) = 2",
            "value:=",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Public Property Let Item(ByVal index As Long, ByVal value As Long)\nEnd Property\n"
                .to_owned(),
            "Let Me.Item(value:=1) = 2",
            "value:=",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Public Property Set Item(ByVal index As Long, ByVal value As Object)\nEnd Property\n"
                .to_owned(),
            "Set Me.Item(value:=1) = Nothing",
            "value:=",
        ),
        ("", method.to_owned(), "Call Me.Visit(size:=1)", "size:="),
        (
            "VERSION 1.0 CLASS\n",
            method.to_owned(),
            "Call [Me].Visit(size:=1)",
            "size:=",
        ),
        (
            "VERSION 1.0 CLASS\n",
            method.repeat(2),
            "Call Me.Visit(size:=1)",
            "size:=",
        ),
        (
            "VERSION 1.0 CLASS\n",
            format!("{method}Public Function Factory() As Object\nEnd Function\n"),
            "Call Me.Factory().Visit(size:=1)",
            "size:=",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private items(2) As Long\n".to_owned(),
            "Call Me.items(size:=1)",
            "size:=",
        ),
        (
            "VERSION 1.0 CLASS\n",
            method.to_owned(),
            "Call Me.Visit(missing:=1)",
            "missing:=",
        ),
    ] {
        let source = format!(
            "{class_header}Attribute VB_Name = \"Receiver\"\n{declarations}Sub Run()\n{invocation}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let label = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.nonvalue_identifier.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        assert!(
            matches!(label.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(document.skipped_regions.iter().any(
            |gap| gap.source == label.source && gap.detail == "vba-nonvalue-target-unavailable"
        ));
    }
}

#[test]
fn vba_nested_named_arguments_follow_their_own_application() {
    let source = "Public Function Inner(ByVal value As Long) As Long\nInner = value\nEnd Function\nPublic Sub Visit(ByVal size As Long, ByVal label As Long)\nEnd Sub\nPublic Sub Run(ByVal size As Long)\nCall Visit(size:=Inner(value:=size), label:=size)\nCall Visit(size:=Len(\"a)\"\"(\"), label:=size)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    for (marker, owner_name, parameter_name) in [
        ("size:=Inner", "Visit", "size"),
        ("value:=size", "Inner", "value"),
        ("label:=size", "Visit", "label"),
    ] {
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner_name)
            .unwrap();
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == parameter_name
                    && entity.container == Some(ContainerRef::Entity(owner.id))
            })
            .unwrap();
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let label = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert_eq!(
            label.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            },
            "{marker}"
        );
    }
    let visit = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Visit")
        .unwrap();
    let parameter = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.canonical_name == "label"
                && entity.container == Some(ContainerRef::Entity(visit.id))
        })
        .unwrap();
    let second_label = u64::try_from(source.rfind("label:=size").unwrap()).unwrap();
    assert!(document.occurrences.iter().any(|site| {
        site.source.span().start_byte() == second_label
            && site.target
                == OccurrenceTarget::Resolved {
                    symbol: parameter.id,
                }
    }));
}

#[test]
fn vba_named_arguments_do_not_guess_ambiguous_or_receiver_callees() {
    for source in [
        "Public Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run()\nCall Visit(size:=1)\nEnd Sub\n",
        "Public Function Factory(ByVal size As Long) As Object\nEnd Function\nPublic Sub Run()\nCall Factory().Visit(size:=1)\nEnd Sub\n",
        "Public Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run()\nVisit size:=1\nEnd Sub\n",
        "Public Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run(ByVal Visit As Object)\nVisit size:=1\nEnd Sub\n",
        "Public Function Factory(ByVal size As Long) As Object\nEnd Function\nPublic Sub Run()\nFactory().Visit size:=1\nEnd Sub\n",
        "Public Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run(ByVal receiver As Object)\nreceiver.Visit size:=1\nEnd Sub\n",
        "Public Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run(ByVal receiver As Object)\nVisit receiver.Visit(size:=1)\nEnd Sub\n",
    ] {
        let result = output(source);
        let start = u64::try_from(source.find("size:=1").unwrap()).unwrap();
        let label = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.nonvalue_identifier.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        assert!(
            matches!(label.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.source == label.source && gap.detail == "vba-nonvalue-target-unavailable"
        }));
    }
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
    assert_eq!(
        condition.syntax_kind,
        "vba.compilation_identifier.reference"
    );
    assert!(matches!(
        condition.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "vba-conditional-compilation-evaluation-unavailable" })
    );
}
