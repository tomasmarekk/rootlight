//! MATLAB source identities through the production parser and normalized IR.
//! Written applications remain distinct from proven runtime function calls.

use super::*;

pub(super) const MATLAB: LanguageCase = LanguageCase {
    name: "matlab",
    path: "src/measure.m",
    frontend: "tree-sitter-matlab-1.3.0",
    source: "function result = measure(value)\nresult = value + 1;\nend\n",
    generated: false,
    body_before: "value + 1",
    body_after: "value + 2",
};

const FUNCTIONS: &str =
    include_str!("../../../../tests/native-grammars/fixtures/matlab/functions.m");
const CLASS: &str = include_str!("../../../../tests/native-grammars/fixtures/matlab/Meter.m");

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(MATLAB, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, MATLAB),
        &request(&fixture.snapshot, &fixture.source, MATLAB, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn reference_in<'a>(
    result: &'a AnalysisOutput,
    source: &str,
    fragment: &str,
    name: &str,
) -> &'a rootlight_ir::OccurrenceRecord {
    let offset = source.find(fragment).unwrap() + fragment.find(name).unwrap();
    result
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == u64::try_from(offset).unwrap()
                && site.source.span().end_byte() == u64::try_from(offset + name.len()).unwrap()
        })
        .unwrap_or_else(|| panic!("missing reference {fragment}"))
}

#[test]
fn matlab_local_functions_bind_names_and_prove_direct_calls() {
    let source = "function result = entry(value)\nresult = helper(value);\ncallback = @helper;\nend\nfunction result = helper(value)\nresult = value;\nend\n";
    let result = output(source);
    let helper = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "helper" && entity.kind == EntityKind::Function)
        .unwrap();
    for fragment in ["helper(value);", "@helper"] {
        assert_eq!(
            reference_in(&result, source, fragment, "helper").target,
            OccurrenceTarget::Resolved { symbol: helper.id }
        );
    }
    let call = result
        .document()
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::CallSite)
        .unwrap();
    assert_eq!(
        call.target,
        OccurrenceTarget::Resolved { symbol: helper.id }
    );
    let span = call.source.span();
    assert_eq!(
        source.get(
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()
        ),
        Some("helper(value)")
    );
    assert!(
        result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(call.id)
                && edge.object == RelationEndpoint::Entity(helper.id))
    );
}

#[test]
fn matlab_duplicate_formal_parameters_do_not_create_exact_bindings() {
    let source = "function result = invalid(value, value)\nresult = value;\nend\n";
    let result = output(source);
    assert!(matches!(
        reference_in(&result, source, "result = value", "value").target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "value")
            .count(),
        2
    );
}

#[test]
fn matlab_nested_function_visibility_tracks_ancestry_and_siblings() {
    let source = "function result = outer(value)\nresult = first(value);\nfunction result = first(value)\nresult = second(value);\nfunction result = hidden(value)\nresult = first(value);\nend\nend\nfunction result = second(value)\nresult = outer(value) + hidden(value);\nend\nend\nfunction result = separate(value)\nresult = first(value);\nend\n";
    let result = output(source);
    for (fragment, name) in [
        ("result = first(value);\nfunction", "first"),
        ("second(value);", "second"),
        ("result = first(value);\nend\nend", "first"),
        ("outer(value) +", "outer"),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == name)
            .unwrap();
        assert_eq!(
            reference_in(&result, source, fragment, name).target,
            OccurrenceTarget::Resolved { symbol: entity.id },
            "{fragment}"
        );
    }
    assert!(matches!(
        reference_in(&result, source, "+ hidden(value)", "hidden").target,
        OccurrenceTarget::Unresolved { .. }
    ));
    let separate = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "separate")
        .unwrap();
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.enclosing == Some(separate.id)
                && site.syntax_kind == "matlab.named_application.reference")
            .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert_eq!(
        result
            .document()
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        4
    );
}

#[test]
fn matlab_function_calls_respect_variable_shadowing_and_duplicate_formals() {
    for header in ["helper", "helper, helper"] {
        let source = format!(
            "function result = entry({header})\nresult = helper(1);\nend\nfunction result = helper(value)\nresult = value;\nend\n"
        );
        let result = output(&source);
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
        let read = reference_in(&result, &source, "helper(1)", "helper");
        if header.contains(',') {
            assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
        } else {
            let parameter = result
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Parameter && entity.canonical_name == "helper"
                })
                .unwrap();
            assert_eq!(
                read.target,
                OccurrenceTarget::Resolved {
                    symbol: parameter.id
                }
            );
        }
    }
}

#[test]
fn matlab_unqualified_handles_select_functions_not_variables_or_members() {
    let source = "function result = entry(helper)\nresult = @helper;\nqualified = @package.helper;\nend\nfunction result = helper(value)\nresult = value;\nend\n";
    let result = output(source);
    let helper = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "helper")
        .unwrap();
    assert_eq!(
        reference_in(&result, source, "@helper", "helper").target,
        OccurrenceTarget::Resolved { symbol: helper.id }
    );
    assert!(matches!(
        reference_in(&result, source, "@package.helper", "helper").target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn matlab_explicit_imports_block_local_targets_but_wildcards_do_not() {
    for (import, expected) in [
        ("import package.helper", false),
        ("import package.other", true),
        ("import package.*", true),
        ("import('package.helper')", false),
        ("import package.other package.helper", false),
        (
            "% import package.helper\ntext = 'import package.helper';",
            true,
        ),
    ] {
        let source = format!(
            "function result = entry(value)\n{import};\nresult = helper(value);\nend\nfunction result = helper(value)\nresult = value;\nend\n"
        );
        let result = output(&source);
        assert_eq!(
            matches!(
                reference_in(&result, &source, "helper(value);", "helper").target,
                OccurrenceTarget::Resolved { .. }
            ),
            expected,
            "{import}"
        );
        assert_eq!(
            result
                .document()
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Calls)
                .count(),
            usize::from(expected),
            "{import}"
        );
    }
}

#[test]
fn matlab_nested_functions_inherit_explicit_imports_not_script_imports() {
    let source = "import package.helper\nvalue = entry(1);\nfunction result = entry(value)\nresult = helper(value);\nend\nfunction result = nested(value)\nimport package.helper\nresult = inner(value);\nfunction result = inner(value)\nresult = helper(value) + value;\nend\nend\nfunction result = helper(value)\nresult = value;\nend\n";
    let result = output(source);
    assert!(matches!(
        reference_in(&result, source, "helper(value);", "helper").target,
        OccurrenceTarget::Resolved { .. }
    ));
    assert!(matches!(
        reference_in(&result, source, "helper(value) +", "helper").target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn matlab_duplicate_function_declarations_block_exact_targets() {
    let source = "function result = entry(value)\nresult = helper(value);\nend\nfunction result = helper(value)\nresult = value;\nend\nfunction result = helper(value)\nresult = value + 1;\nend\n";
    let result = output(source);
    assert!(matches!(
        reference_in(&result, source, "helper(value);", "helper").target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn matlab_member_brace_and_superclass_applications_are_not_local_calls() {
    for application in ["object.helper(1)", "helper{1}", "helper@Base(1)"] {
        let source = format!(
            "function result = entry(object)\nresult = {application};\nend\nfunction result = helper(value)\nresult = value;\nend\n"
        );
        let result = output(&source);
        assert!(
            matches!(
                reference_in(&result, &source, application, "helper").target,
                OccurrenceTarget::Unresolved { .. }
            ),
            "{application}"
        );
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls),
            "{application}"
        );
    }
}

#[test]
fn matlab_chained_application_proves_only_the_named_inner_call() {
    let source = "function result = entry(value)\nresult = helper(value)(1);\nend\nfunction result = helper(value)\nresult = value;\nend\n";
    let result = output(source);
    let calls: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1);
    let span = calls[0].source.span();
    assert_eq!(
        source.get(
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()
        ),
        Some("helper(value)")
    );
}

#[test]
fn matlab_continued_calls_keep_exact_source_and_ignore_member_import_names() {
    let source = "function result = entry(value)\nvalue.import('package.helper');\nresult = helper ... continued call\n(value);\nend\nfunction result = helper(value)\nresult = value;\nend\n";
    let result = output(source);
    let calls: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1);
    let span = calls[0].source.span();
    assert_eq!(
        source.get(
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()
        ),
        Some("helper ... continued call\n(value)")
    );
}

#[test]
fn matlab_bounded_capture_plans_do_not_guess_lexical_targets() {
    let source = format!(
        "function result = sum_values(value)\nresult = helper(value) + {};\nend\nfunction out = helper(item)\nout = item;\nend\n",
        vec!["value"; 80].join(" + ")
    );
    let provider = Arc::new(provider());
    let fixture = Fixture::new(MATLAB, source.as_bytes());
    let budget = limits_with_syntax_records(64);
    let result = analyze(
        &analyzer(&provider, MATLAB),
        &request(&fixture.snapshot, &fixture.source, MATLAB, &budget),
        &ExtensionSupport::default(),
    );
    assert_eq!(result.report().coverage().status(), CoverageStatus::Bounded);
    let reads: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference)
        .collect();
    assert!(!reads.is_empty());
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
    assert!(
        reads
            .iter()
            .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
}

#[test]
fn matlab_binding_identities_survive_trivia_and_unrelated_sibling_insertions() {
    let source = "function result = accumulate(value)\nlocal = value;\nlocal = local + 1;\nresult = local;\nend\n";
    let original = output(source);
    let changed = format!(
        "% λ😀\nfunction other = unrelated(input)\nother = input;\nend\n{}",
        source.replace("+ 1", "+ 2")
    );
    let updated = output(&changed);
    for entity in &original.document().entities {
        if !matches!(
            entity.canonical_name.as_str(),
            "accumulate" | "result" | "value" | "local"
        ) {
            continue;
        }
        let counterpart = updated
            .document()
            .entities
            .iter()
            .find(|candidate| candidate.canonical_name == entity.canonical_name)
            .unwrap();
        assert_eq!(entity.id, counterpart.id, "{}", entity.canonical_name);
        for site in updated.document().occurrences.iter().filter(|site| {
            site.target
                == OccurrenceTarget::Resolved {
                    symbol: counterpart.id,
                }
        }) {
            let span = site.source.span();
            assert_eq!(
                changed.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some(entity.canonical_name.as_str())
            );
        }
    }
}

#[test]
fn matlab_anonymous_functions_capture_outer_names_and_shadow_inputs() {
    let source = "function result = factory(value)\nscale = value;\ncallback = @(value) value + scale;\nresult = callback(value);\nend\n";
    let result = output(source);
    let outer = reference_in(&result, source, "scale = value", "value");
    let inner = reference_in(&result, source, "value + scale", "value");
    assert!(matches!(outer.target, OccurrenceTarget::Resolved { .. }));
    assert!(matches!(inner.target, OccurrenceTarget::Resolved { .. }));
    assert_ne!(outer.target, inner.target);
    assert_eq!(
        outer.target,
        reference_in(&result, source, "callback(value)", "value").target
    );
    let scale = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "scale")
        .unwrap();
    assert_eq!(
        reference_in(&result, source, "+ scale", "scale").target,
        OccurrenceTarget::Resolved { symbol: scale.id }
    );
}

#[test]
fn matlab_script_variables_are_not_captured_by_local_functions() {
    let source =
        "value = 1;\ncallback = @() value;\nfunction result = isolated\nresult = value;\nend\n";
    let result = output(source);
    assert!(matches!(
        reference_in(&result, source, "@() value", "value").target,
        OccurrenceTarget::Resolved { .. }
    ));
    assert!(matches!(
        reference_in(&result, source, "result = value", "value").target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn matlab_loop_and_catch_names_keep_function_workspace_identity() {
    let source = "function result = accumulate(values)\nresult = 0;\nfor item = values\nresult = result + item;\nend\ntry\nresult = result + 1;\ncatch problem\nresult = problem.message;\nend\nend\n";
    let result = output(source);
    for (name, fragment) in [("item", "+ item"), ("problem", "problem.message")] {
        let entities: Vec<_> = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect();
        assert_eq!(entities.len(), 1, "{name}");
        assert_eq!(entities[0].kind, EntityKind::Variable);
        assert_eq!(
            reference_in(&result, source, fragment, name).target,
            OccurrenceTarget::Resolved {
                symbol: entities[0].id
            }
        );
    }
}

#[test]
fn matlab_persistent_and_global_declarations_keep_local_source_bindings() {
    let source = "function result = first\npersistent cache\nglobal shared\ncache = 1;\nresult = cache + shared;\nend\nfunction result = second\npersistent cache\nglobal shared\ncache = 2;\nresult = shared + cache;\nend\n";
    let result = output(source);
    for name in ["cache", "shared"] {
        let entities: Vec<_> = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect();
        assert_eq!(entities.len(), 2, "{name}");
        let first = reference_in(&result, source, "cache + shared", name);
        let second = reference_in(&result, source, "shared + cache", name);
        assert!(matches!(first.target, OccurrenceTarget::Resolved { .. }));
        assert!(matches!(second.target, OccurrenceTarget::Resolved { .. }));
        assert_ne!(first.target, second.target);
    }
}

#[test]
fn matlab_repeated_assignments_share_one_variable_and_keep_every_write() {
    let source = "function result = accumulate(value)\nresult = value;\nresult = result + 1;\nvalue = result;\nend\n";
    let result = output(source);
    for (name, kind, writes) in [
        ("result", EntityKind::Variable, 2),
        ("value", EntityKind::Parameter, 1),
    ] {
        let entities: Vec<_> = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect();
        assert_eq!(entities.len(), 1, "{name}");
        assert_eq!(entities[0].kind, kind);
        let sites: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.target
                    == OccurrenceTarget::Resolved {
                        symbol: entities[0].id,
                    }
            })
            .collect();
        assert_eq!(
            sites
                .iter()
                .filter(|site| site.role == OccurrenceRole::Definition)
                .count(),
            1
        );
        assert_eq!(
            sites
                .iter()
                .filter(|site| site.role == OccurrenceRole::Write)
                .count(),
            writes
        );
        assert!(
            sites
                .iter()
                .any(|site| site.role == OccurrenceRole::Reference)
        );
        for site in sites {
            let span = site.source.span();
            assert_eq!(
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
                name
            );
        }
    }
}

#[test]
fn matlab_local_functions_keep_same_named_parameters_separate() {
    let source = "function result = first(value)\nresult = value;\nend\nfunction result = second(value)\nresult = value;\nend\n";
    let result = output(source);
    let parameters: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "value")
        .collect();
    assert_eq!(parameters.len(), 2);
    assert_ne!(parameters[0].id, parameters[1].id);
    for parameter in parameters {
        let reads: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        }
            })
            .collect();
        assert_eq!(reads.len(), 1);
        let Some(rootlight_ir::ContainerRef::Entity(owner)) = parameter.container else {
            panic!("parameter requires a function owner")
        };
        assert_eq!(reads[0].enclosing, Some(owner));
    }
}

#[test]
fn matlab_member_and_function_handle_names_do_not_bind_local_namesakes() {
    let source = "function result = inspect(object)\nmember = 1;\nresult = object.member + member;\ncallback = @member;\nend\n";
    let result = output(source);
    for (fragment, name, should_resolve) in [
        ("object.member", "object", true),
        (".member", "member", false),
        ("+ member", "member", true),
        ("@member", "member", false),
    ] {
        let offset = source.find(fragment).unwrap() + fragment.find(name).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == u64::try_from(offset).unwrap()
            })
            .unwrap();
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            should_resolve,
            "{fragment}"
        );
    }
}

#[test]
fn matlab_nested_workspaces_share_outer_writes_but_isolate_parameters_and_siblings() {
    let source = "function result = outer(value)\nshared = value;\nresult = inner(value);\nfunction result = inner(value)\nshared = shared + value;\nresult = shared;\nend\nfunction first\nprivate = 1;\ndisp(private);\nend\nfunction second\nprivate = 2;\ndisp(private);\nend\nend\n";
    let result = output(source);
    for (name, count) in [("shared", 1), ("value", 2), ("result", 2), ("private", 2)] {
        assert_eq!(
            result
                .document()
                .entities
                .iter()
                .filter(|entity| entity.canonical_name == name)
                .count(),
            count,
            "{name}"
        );
    }
    let shared = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "shared")
        .unwrap();
    let inner = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "inner")
        .unwrap();
    let writes: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Write
                && site.target == OccurrenceTarget::Resolved { symbol: shared.id }
        })
        .collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].enclosing, Some(inner.id));
    let reads: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.target == OccurrenceTarget::Resolved { symbol: shared.id }
        })
        .collect();
    assert_eq!(reads.len(), 2);
    assert!(reads.iter().all(|site| site.enclosing == Some(inner.id)));
}

#[test]
fn matlab_indexed_and_multiple_assignment_roots_are_variables_not_member_names() {
    let source = "function result = collect(index, input)\n[first, second] = split(input);\nitems(index).value = first;\nitems(index).value = second;\nresult = items(index).value;\nend\n";
    let result = output(source);
    for name in ["first", "second", "items"] {
        assert_eq!(
            result
                .document()
                .entities
                .iter()
                .filter(
                    |entity| entity.kind == EntityKind::Variable && entity.canonical_name == name
                )
                .count(),
            1,
            "{name}"
        );
    }
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "value")
    );
    let index = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "index")
        .unwrap();
    assert_eq!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference
                && site.target == OccurrenceTarget::Resolved { symbol: index.id })
            .count(),
        3
    );
}

#[test]
fn matlab_shared_input_output_name_remains_one_parameter_binding() {
    let source = "function value = update(value)\nvalue = value + 1;\nend\n";
    let result = output(source);
    let variables: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "value")
        .collect();
    assert_eq!(variables.len(), 1);
    assert_eq!(variables[0].kind, EntityKind::Parameter);
}

#[test]
fn matlab_functions_and_parameters_have_real_definition_sources() {
    let result = output(MATLAB.source);
    let entities = &result.document().entities;
    assert!(entities.iter().any(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "measure"));
    assert!(
        entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "value")
    );
    for name in ["measure", "value"] {
        let entity = entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let definitions: Vec<_> = result
            .document()
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
            &MATLAB.source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
    }
}

#[test]
fn matlab_class_members_and_accessors_have_exact_written_definitions() {
    let result = output(CLASS);
    for (name, kind) in [
        ("Meter", EntityKind::Class),
        ("Meter", EntityKind::Constructor),
        ("Value", EntityKind::Property),
        ("read", EntityKind::Method),
        ("get.Value", EntityKind::Method),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == kind && entity.canonical_name == name)
            .unwrap_or_else(|| panic!("missing {kind:?} {name}: {:?}", result.document().entities));
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            &CLASS[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
    }
}

#[test]
fn matlab_nested_functions_and_parameters_exclude_lexical_decoys() {
    for source in [FUNCTIONS.to_owned(), FUNCTIONS.replace('\n', "\r\n")] {
        let result = output(&source);
        for (name, kind) in [
            ("summarize", EntityKind::Function),
            ("adjust", EntityKind::Function),
            ("identity", EntityKind::Function),
            ("values", EntityKind::Parameter),
            ("scale", EntityKind::Parameter),
            ("item", EntityKind::Parameter),
            ("count", EntityKind::Variable),
        ] {
            assert!(
                result
                    .document()
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == name && entity.kind == kind),
                "missing {kind:?} {name}: {:?}",
                result
                    .document()
                    .entities
                    .iter()
                    .map(|entity| (&entity.canonical_name, entity.kind))
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| matches!(entity.canonical_name.as_str(), "fake" | "hidden"))
        );
        for occurrence in result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
        {
            let span = occurrence.source.span();
            let written = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(written.as_bytes())
            );
        }
    }
}

#[test]
fn matlab_signatures_exclude_bodies_and_preserve_accessor_prefixes() {
    for (source, expected) in [
        (MATLAB.source, vec!["function result = measure(value)"]),
        (
            CLASS,
            vec![
                "classdef Meter < handle",
                "function obj = Meter(value)",
                "function result = read(obj)",
                "function result = get.Value(obj)",
            ],
        ),
    ] {
        let fixture = Fixture::new(MATLAB, source.as_bytes());
        let budget = limits();
        let request = request(&fixture.snapshot, &fixture.source, MATLAB, &budget);
        let result = rootlight_adapter_sdk::execute_parse(
            &provider(),
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let signatures: Vec<_> = result
            .facts()
            .iter()
            .filter(|fact| fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature)
            .map(|fact| {
                let span = fact.span();
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()]
            })
            .collect();
        assert_eq!(signatures, expected);
    }
}

#[test]
fn matlab_multiple_outputs_keep_the_function_name_capture() {
    let source =
        "function [first, second] = partition(values)\nfirst = values;\nsecond = values;\nend\n";
    let result = output(source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Function
                && entity.canonical_name == "partition"),
        "{:?}",
        result
            .document()
            .entities
            .iter()
            .map(|entity| (&entity.canonical_name, entity.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn matlab_body_edits_preserve_written_symbol_identity() {
    let original = output(MATLAB.source);
    let updated = output(&MATLAB.source.replace(MATLAB.body_before, MATLAB.body_after));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| {
                (
                    entity.id,
                    (entity.kind, entity.canonical_name.clone(), entity.container),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&original), identities(&updated));
}

#[test]
fn matlab_applications_do_not_claim_calls_without_binding_evidence() {
    let source = "function result = select(values)\nresult = values(1);\ndisp result\nend\n";
    let result = output(source);
    let applications: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "matlab.named_application.reference")
        .collect();
    assert_eq!(applications.len(), 1);
    assert!(matches!(
        applications[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::CallSite)
    );
    for detail in [
        "matlab-call-or-index-target-unavailable",
        "matlab-command-target-unavailable",
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == detail),
            "{detail}"
        );
    }
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn matlab_artifact_replay_matches_fresh_ir_in_the_new_generation() {
    for source in [
        FUNCTIONS,
        CLASS,
        "function result = outer(value)\nshared = value;\nresult = inner(value);\nfunction result = inner(value)\nshared = shared + value;\nresult = shared;\nend\nend\n",
    ] {
        let provider = Arc::new(provider());
        let budget = limits();
        let fixture = Fixture::new(MATLAB, source.as_bytes());
        let analyzer = analyzer(&provider, MATLAB);
        let (_, artifact) = analyzer
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, MATLAB, &budget),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let successor = fixture.next_generation();
        let request = request(&successor.snapshot, &successor.source, MATLAB, &budget);
        let reused = analyzer
            .analyze_from_artifact(
                &request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let fresh = analyze(&analyzer, &request, &ExtensionSupport::default());
        assert_eq!(reused.document(), fresh.document());
        assert_eq!(reused.report(), fresh.report());
        assert!(
            reused
                .document()
                .occurrences
                .iter()
                .all(|occurrence| occurrence.source.generation() == successor.source.generation())
        );
    }
}
