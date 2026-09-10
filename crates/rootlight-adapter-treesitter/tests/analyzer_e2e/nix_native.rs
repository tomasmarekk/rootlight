//! Nix source bindings through the public analyzer and validated durable IR.
//! Dynamic evaluation must not masquerade as a source-backed definition or relation.

use super::*;

pub(super) const NIX: LanguageCase = LanguageCase {
    name: "nix",
    path: "src/module.nix",
    frontend: "tree-sitter-nix-0.3.0",
    source: include_str!("../../../../tests/fixtures/nix/expressions.nix"),
    generated: false,
    body_before: "first + 1",
    body_after: "first + 2",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(NIX, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, NIX),
        &request(&fixture.snapshot, &fixture.source, NIX, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn assert_bindings(source: &str, cases: &[(&str, &str, Option<&str>)]) {
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for &(context, name, definition_context) in cases {
        let start = source.find(context).unwrap() + context.find(name).unwrap();
        let reference = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Reference
                    && occurrence.source.span().start_byte() == u64::try_from(start).unwrap()
                    && occurrence.source.span().end_byte()
                        == u64::try_from(start + name.len()).unwrap()
            })
            .unwrap_or_else(|| panic!("missing read {context}: {:?}", document.occurrences));
        if let Some(definition_context) = definition_context {
            let start =
                source.find(definition_context).unwrap() + definition_context.find(name).unwrap();
            let definition = document
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.role == OccurrenceRole::Definition
                        && occurrence.source.span().start_byte() == u64::try_from(start).unwrap()
                })
                .unwrap_or_else(|| panic!("missing definition {definition_context}"));
            let OccurrenceTarget::Resolved { symbol } = definition.target else {
                panic!("definition must have a source-backed identity");
            };
            assert_eq!(
                reference.target,
                OccurrenceTarget::Resolved { symbol },
                "{context}"
            );
            assert!(
                document.relations.iter().any(|relation| {
                    relation.predicate == rootlight_ir::RelationPredicate::RefersTo
                        && relation.subject
                            == rootlight_ir::RelationEndpoint::Occurrence(reference.id)
                        && relation.object == rootlight_ir::RelationEndpoint::Entity(symbol)
                }),
                "missing exact relation for {context}"
            );
        } else {
            assert!(
                matches!(reference.target, OccurrenceTarget::Unresolved { .. }),
                "{context}: {reference:?}"
            );
        }
    }
}

#[test]
fn nix_literal_interpolated_keys_shadow_outer_names_with_exact_sources() {
    for key in [
        r#"${"name"}"#,
        r#"${ ( /* key */ "n\ame" ) }"#,
        "${''name''}",
        r#"${''${"name"}''}"#,
    ] {
        let source = format!("let name = 0; in rec {{ {key} = 1; result = name; }}");
        let result = output(&source);
        let target = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == key)
            .unwrap_or_else(|| {
                panic!(
                    "missing literal-key definition: {:?}",
                    result.document().entities
                )
            });
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|item| {
                item.role == OccurrenceRole::Definition
                    && item.target == (OccurrenceTarget::Resolved { symbol: target.id })
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(key)
        );
        let start = source.rfind("name;").unwrap();
        let reference = result
            .document()
            .occurrences
            .iter()
            .find(|item| {
                item.role == OccurrenceRole::Reference
                    && item.source.span().start_byte() == u64::try_from(start).unwrap()
            })
            .unwrap();
        assert_eq!(
            reference.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_nix_artifact_replay(&source);
    }
}

#[test]
fn nix_evaluated_string_keys_do_not_create_lexical_bindings() {
    for key in [
        r#""${"name"}""#,
        r#"${"na" + "me"}"#,
        r#"${''${"name"} ''}"#,
        r#"${''${"name"}\n''}"#,
    ] {
        let source = format!("let name = 0; in rec {{ {key} = 1; result = name; }}");
        assert_bindings(&source, &[("result = name;", "name", Some("name = 0"))]);
    }
}

#[test]
fn nix_literal_nonidentifier_keys_have_exact_written_definitions() {
    for key in [r#"${scheme:path}"#, "${''\n  λ😀\n  ''}", r#"${""}"#] {
        let source = format!("{{ {key} = 1; }}");
        let result = output(&source);
        let entity = result
            .document()
            .entities
            .iter()
            .find(|item| item.canonical_name == key)
            .unwrap();
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|item| {
                item.role == OccurrenceRole::Definition
                    && item.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(key)
        );
        assert_nix_artifact_replay(&source);
    }
}

#[test]
fn nix_literal_prefixes_share_written_attribute_namespaces() {
    let source = r#"let ${"name"}.item = 1; name.other = 2; in name"#;
    let result = output(source);
    let roots: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|item| item.canonical_name == "name")
        .collect();
    assert_eq!(roots.len(), 1);
    let read_start = u64::try_from(source.rfind("name").unwrap()).unwrap();
    let read = result
        .document()
        .occurrences
        .iter()
        .find(|item| {
            item.role == OccurrenceRole::Reference && item.source.span().start_byte() == read_start
        })
        .unwrap();
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved {
            symbol: roots[0].id
        }
    );
    assert_nix_artifact_replay(source);
    assert_bindings(
        r#"let ${"name"} = rec { part = 1; }; name.added = part; in name"#,
        &[("added = part;", "part", Some("part = 1"))],
    );
}

#[test]
fn nix_merged_attribute_sets_follow_the_first_sets_recursion_mode() {
    for source in [
        "let a = { b = 1; }; a.c = 2; in a",
        "let a.b = 1; a = { c = 2; }; in a",
        "let a = { b = 1; }; a = { c = 2; }; in a",
    ] {
        let result = output(source);
        let roots: Vec<_> = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "a")
            .collect();
        assert_eq!(
            roots.len(),
            1,
            "merged root in {source}: {:?}",
            result.document().entities
        );
        let reference = result
            .document()
            .occurrences
            .iter()
            .find(|item| {
                item.role == OccurrenceRole::Reference
                    && item.source.span().end_byte() == u64::try_from(source.len()).unwrap()
            })
            .unwrap();
        assert_eq!(
            reference.target,
            OccurrenceTarget::Resolved {
                symbol: roots[0].id
            }
        );
    }
    assert_bindings(
        "let x = 0; a = rec { x = 1; }; a.y = x; in a",
        &[("y = x", "x", Some("x = 1"))],
    );
    assert_bindings(
        "let x = 0; a.y = x; a = rec { x = 1; z = x; }; in a",
        &[("y = x", "x", Some("x = 0")), ("z = x", "x", Some("x = 0"))],
    );
}

#[test]
fn nix_nested_set_merges_preserve_inheritance_and_reject_scalar_conflicts() {
    for source in [
        "let x = 0; a = rec { nested = rec { x = 1; }; }; a.nested.y = x; in a",
        "let x = 0; a.nested = rec { x = 1; }; a = { nested.y = x; }; in a",
        "let x = 0; a = ((rec { x = 1; })); a.y = x; in a",
        "let x = 0; a = rec { x = 1; }; a = { y = x; }; in a",
    ] {
        assert_bindings(source, &[("y = x", "x", Some("x = 1"))]);
        assert_nix_artifact_replay(source);
    }
    assert_bindings(
        "let x = 0; a = rec { z = 1; }; a = { inherit x; }; in a",
        &[("inherit x", "x", Some("x = 0"))],
    );
    for source in [
        "let x = 0; a = 1; a.x = 2; in a",
        "let x = 0; a.x = 2; a = 1; in a",
        "let x = 0; a = (let local = 1; in { x = local; }); a.y = 2; in a",
    ] {
        assert_bindings(source, &[("in a", "a", None)]);
    }
}

#[test]
fn nix_attribute_path_roots_merge_written_sites_without_becoming_callable_leaves() {
    let source = "let a.b = value: value; a.c = 2; in a";
    let result = output(source);
    let document = result.document();
    let roots: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "a")
        .collect();
    assert_eq!(
        roots.len(),
        1,
        "missing unique implicit root: {:?}",
        document.entities
    );
    let root = roots[0];
    assert_eq!(root.kind, EntityKind::Variable);
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == (OccurrenceTarget::Resolved { symbol: root.id })
        })
        .collect();
    assert_eq!(definitions.len(), 2);
    for definition in definitions {
        let span = definition.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("a")
        );
    }
    let reference = document
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::Reference
                && occurrence.source.span().end_byte() == u64::try_from(source.len()).unwrap()
        })
        .unwrap();
    assert_eq!(
        reference.target,
        OccurrenceTarget::Resolved { symbol: root.id }
    );
    let leaf = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "a.b")
        .unwrap();
    assert_eq!(leaf.kind, EntityKind::Function);
    assert_ne!(leaf.id, root.id);
    assert_eq!(
        leaf.container,
        Some(rootlight_ir::ContainerRef::Entity(root.id))
    );
}

#[test]
fn nix_lexical_bindings_follow_recursive_scope_and_plain_attribute_boundaries() {
    let source = "let x = 1; forward = later; later = 2; in { x = 3; plain = x; nested = rec { x = 4; own = x; }; shadow = let x = 5; in x; old = let { body = x; x = 6; }; }";
    assert_bindings(
        source,
        &[
            ("forward = later", "later", Some("later = 2")),
            ("plain = x", "x", Some("x = 1")),
            ("own = x", "x", Some("x = 4")),
            ("in x;", "x", Some("x = 5")),
            ("body = x", "x", Some("x = 6")),
        ],
    );
}

#[test]
fn nix_lexical_bindings_cover_defaults_at_patterns_and_shadowed_parameters() {
    let source = "let x = 0; in args@{ x ? y, y ? args }: [ (x: x + 1) x y args ]";
    assert_bindings(
        source,
        &[
            ("x ? y", "y", Some("y ? args")),
            ("y ? args", "args", Some("args@")),
            ("x + 1", "x", Some("x: x")),
            (") x y", "x", Some("x ? y")),
            ("y args ]", "y", Some("y ? args")),
            ("args ]", "args", Some("args@")),
        ],
    );
}

#[test]
fn nix_lexical_bindings_distinguish_inherit_reads_from_attribute_selection() {
    let source = "let x = 1; in let inherit x; inherit (provider) member; in rec { inherit x; result = x; selected = member; }";
    assert_bindings(
        source,
        &[
            ("inherit x; inherit", "x", Some("x = 1")),
            ("inherit x; result", "x", Some("inherit x; inherit")),
            ("result = x", "x", Some("inherit x; result")),
            ("selected = member", "member", Some("member; in")),
            ("provider)", "provider", None),
        ],
    );
}

#[test]
fn nix_lexical_bindings_dominate_with_without_inventing_dynamic_targets() {
    let source = "let x = 1; in with { x = 2; dynamic = 3; }; [ x dynamic missing ]";
    assert_bindings(
        source,
        &[
            ("[ x", "x", Some("x = 1")),
            ("dynamic missing", "dynamic", None),
            ("missing ]", "missing", None),
        ],
    );
}

#[test]
fn nix_lexical_path_roots_shadow_outer_names_even_with_computed_suffixes() {
    for source in [
        "let x = 1; in let x.part = 2; in x",
        "let x = 1; in rec { x.${key} = 2; result = x; }",
    ] {
        let context = if source.ends_with('x') {
            "in x"
        } else {
            "result = x"
        };
        let result = output(source);
        let reference_start = source.find(context).unwrap() + context.find('x').unwrap();
        let reference = result
            .document()
            .occurrences
            .iter()
            .find(|item| {
                item.role == OccurrenceRole::Reference
                    && item.source.span().start_byte() == u64::try_from(reference_start).unwrap()
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = reference.target else {
            panic!("missing implicit root: {reference:?}")
        };
        let root = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(root.canonical_name, "x");
        let span = root.evidence.source.as_ref().unwrap().span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("x")
        );
        assert!(span.start_byte() > 10, "must not resolve the outer x");
    }
}

#[test]
fn nix_implicit_roots_replay_and_retain_identity_across_trivia_and_value_edits() {
    let original = "let a.b.c = 1; a.b.d = 2; in a";
    let changed = "let a /* owner */ . b.c = 10; a.b.d = 20; in a";
    assert_nix_artifact_replay(original);
    let first = output(original);
    let second = output(changed);
    for name in ["a", "a.b"] {
        let first = first
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let second = second
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(first.id, second.id);
    }
    let dynamic = output("let ${key}.part = 1; in missing");
    assert!(
        dynamic
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "nix-implicit-attribute-owner-unavailable")
    );
}

#[test]
fn nix_nested_quoted_prefixes_merge_but_separate_lexical_scopes_do_not() {
    let source = r#"let a."b".c = 1; "\a".b.d = 2; in a"#;
    let result = output(source);
    for name in ["a", "a.b"] {
        let roots: Vec<_> = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect();
        assert_eq!(roots.len(), 1, "{name}");
        assert_eq!(
            result
                .document()
                .occurrences
                .iter()
                .filter(|item| item.role == OccurrenceRole::Definition
                    && item.target
                        == (OccurrenceTarget::Resolved {
                            symbol: roots[0].id
                        }))
                .count(),
            2
        );
    }
    let separate = output("[ (let a.b = 1; in a) (let a.c = 2; in a) ]");
    let roots: Vec<_> = separate
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "a")
        .collect();
    assert_eq!(roots.len(), 2, "separate lexical roots must not merge");
    assert_ne!(roots[0].id, roots[1].id);
    assert_bindings(
        "let x = 1; in { x.part = 2; result = x; }",
        &[("result = x", "x", Some("x = 1"))],
    );
    assert_bindings(
        "let x = 1; in let x = 2; x.part = 3; in x",
        &[("in x", "x", None)],
    );
}

#[test]
fn nix_quoted_lexical_names_resolve_to_exact_authored_definitions() {
    for (source, read, written) in [
        (r#"let x = 1; in let "x" = 2; in x"#, "x", r#""x""#),
        (
            r#"let x = 1; in rec { "\x" = 2; result = x; }"#,
            "x",
            r#""\x""#,
        ),
        (r#"let "x" = 1; in { inherit x; }"#, "x", r#""x""#),
        (r#"let x = 1; in { inherit "\x"; }"#, r#""\x""#, "x"),
        (
            r#"let "a.b" = 1; in { inherit "a.b"; }"#,
            r#""a.b""#,
            r#""a.b""#,
        ),
        (r#"let "" = 1; in { inherit ""; }"#, r#""""#, r#""""#),
        (
            r#"let "λ😀" = 1; in { inherit "λ😀"; }"#,
            r#""λ😀""#,
            r#""λ😀""#,
        ),
    ] {
        let result = output(source);
        let document = result.document();
        assert!(
            document.diagnostics.is_empty(),
            "{:?}",
            document.diagnostics
        );
        let start = source.find(written).unwrap();
        let definition = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.source.span().start_byte() == u64::try_from(start).unwrap()
                    && occurrence.source.span().end_byte()
                        == u64::try_from(start + written.len()).unwrap()
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = definition.target else {
            panic!("missing authored definition: {source}");
        };
        let start = source.rfind(read).unwrap();
        let reference = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Reference
                    && occurrence.source.span().start_byte() == u64::try_from(start).unwrap()
                    && occurrence.source.span().end_byte()
                        == u64::try_from(start + read.len()).unwrap()
            })
            .unwrap();
        assert_eq!(
            reference.target,
            OccurrenceTarget::Resolved { symbol },
            "{source}"
        );
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == rootlight_ir::RelationPredicate::RefersTo
                && relation.subject == rootlight_ir::RelationEndpoint::Occurrence(reference.id)
                && relation.object == rootlight_ir::RelationEndpoint::Entity(symbol)
        }));
    }
}

#[test]
fn nix_quoted_nonidentifiers_do_not_hide_unrelated_lexical_bindings() {
    for key in [r#""x.y""#, r#""""#, r#""λ😀""#, r#""\n""#, r#""\u0078""#] {
        let source = format!("let x = 1; in let {key} = 2; in x");
        assert_bindings(&source, &[("in x", "x", Some("x = 1"))]);
    }
}

#[test]
fn nix_literal_dollar_pairs_are_not_treated_as_interpolation() {
    let source = r#"let "$${literal}" = 1; in { inherit "$${literal}"; }"#;
    assert_bindings(
        source,
        &[(
            r#"inherit "$${literal}""#,
            r#""$${literal}""#,
            Some(r#""$${literal}" = 1"#),
        )],
    );
}

#[test]
fn nix_bounded_capture_plans_keep_reads_without_claiming_exact_bindings() {
    let source = format!("let x = 1; in [ {} ]", "x ".repeat(80));
    let provider = Arc::new(provider());
    let budget = limits_with_syntax_records(32);
    let fixture = Fixture::new(NIX, source.as_bytes());
    let result = analyze(
        &analyzer(&provider, NIX),
        &request(&fixture.snapshot, &fixture.source, NIX, &budget),
        &ExtensionSupport::default(),
    );
    let reads: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect();
    assert!(!reads.is_empty());
    assert!(
        reads
            .iter()
            .all(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert_eq!(result.report().coverage().status(), CoverageStatus::Bounded);
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "nix-lexical-binding-target-unavailable")
    );
}

#[test]
fn nix_dynamic_keys_and_callee_reads_preserve_lexical_identity_not_runtime_values() {
    let source = "let key = \"outer\"; import = input: input; in rec { key = \"inner\"; ${key} = import ./module.nix; }";
    assert_bindings(
        source,
        &[
            ("${key}", "key", Some("key = \"inner\"")),
            ("import ./", "import", Some("import = input")),
            ("input;", "input", Some("input: input")),
        ],
    );
    let result = output(source);
    let calls: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1);
    assert!(matches!(
        calls[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn nix_native_retains_authored_bindings_and_parameters_without_string_phantoms() {
    let result = output(NIX.source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("choose", EntityKind::Function),
        ("nested", EntityKind::Variable),
        ("first", EntityKind::Variable),
        ("second", EntityKind::Variable),
        ("package.name", EntityKind::Variable),
        ("package.run", EntityKind::Function),
        ("\"quoted.key\"", EntityKind::Variable),
        ("path", EntityKind::Variable),
        ("script", EntityKind::Variable),
        ("lib", EntityKind::Parameter),
        ("system", EntityKind::Parameter),
        ("args", EntityKind::Parameter),
        ("value", EntityKind::Parameter),
        ("name", EntityKind::Parameter),
    ] {
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name && entity.kind == kind),
            "missing {name}: {:?}",
            document.entities
        );
    }
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| ["phantom", "literal", "dynamic"]
                .contains(&entity.canonical_name.as_str()))
    );
    for entity in document
        .entities
        .iter()
        .filter(|entity| !entity.flags.contains(&EntityFlag::Synthetic))
    {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect();
        assert!(!definitions.is_empty(), "missing definition: {entity:?}");
        for definition in definitions {
            let span = definition.source.span();
            assert_eq!(
                NIX.source.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some(entity.canonical_name.as_str())
            );
        }
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "nix-computed-attribute-name-unavailable")
    );
}

#[test]
fn nix_empty_inheritance_never_invents_bindings() {
    for source in [
        "{ inherit; }",
        "{ inherit (builtins); }",
        "let inherit; in 1",
    ] {
        let result = output(source);
        assert!(result.document().diagnostics.is_empty());
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.flags.contains(&EntityFlag::Synthetic))
        );
    }
}

#[test]
fn nix_structural_artifact_rebinds_to_the_same_ir_as_a_clean_generation() {
    assert_nix_artifact_replay(NIX.source);
}

#[test]
fn nix_static_search_names_survive_artifact_replay_with_original_source_identity() {
    let source = r#"{ "\item" = 1; a /* trivia */ . "b" = 2; "a.b" = 3; "λ😀" = 4; }"#;
    let result = output(source);
    for (written, display) in [
        (r#""\item""#, "item"),
        (r#"a /* trivia */ . "b""#, "a.b"),
        (r#""a.b""#, r#""a.b""#),
        (r#""λ😀""#, "λ😀"),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == written)
            .unwrap();
        assert_eq!(entity.display_name, display);
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
    }
    assert_nix_artifact_replay(source);
}

fn assert_nix_artifact_replay(source: &str) {
    let provider = Arc::new(provider());
    let budget = limits();
    let fixture = Fixture::new(NIX, source.as_bytes());
    let analyzer = analyzer(&provider, NIX);
    let initial = request(&fixture.snapshot, &fixture.source, NIX, &budget);
    let (_, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert!(artifact.required_syntax_fact_count(&deadline()).unwrap() > 0);
    let successor = fixture.next_generation();
    let request = request(&successor.snapshot, &successor.source, NIX, &budget);
    let reused = analyzer
        .analyze_from_artifact(
            &request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let clean = analyze(&analyzer, &request, &ExtensionSupport::default());
    assert_eq!(reused.document(), clean.document());
    assert_eq!(reused.report(), clean.report());
}

#[test]
fn nix_source_owners_survive_body_and_trivia_edits_with_distinct_sibling_bindings() {
    let source =
        "let first = { shared = x: x; }; second = { shared = x: x + 1; }; in [ first second ]";
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
    let initial = output(source);
    assert_eq!(
        initial.document().entities.len(),
        7,
        "{:?}",
        initial.document().entities
    );
    assert_eq!(identities(&initial).len(), 7);
    for changed in [
        source.to_owned(),
        format!("# shifted λ😀\r\n{source}"),
        source.replace("x + 1", "x + 2"),
    ] {
        assert_eq!(identities(&output(&changed)), identities(&initial));
    }
}

#[test]
fn nix_source_bindings_do_not_invent_attribute_or_runtime_call_targets() {
    let source = "let outer = 1; inherit (builtins) map; in with environment; { outer = 2; plain = outer; recursive = rec { value = value; }; \"quoted.key\" = outer; quoted.key = outer; selected = object.member; applied = map (x: x) []; }";
    let result = output(source);
    let document = result.document();
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "\"quoted.key\"")
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "quoted.key")
    );
    for occurrence in document.occurrences.iter().filter(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            || occurrence.syntax_kind == "nix.member_name.reference"
    }) {
        assert!(matches!(
            occurrence.target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail
                == "nix-attribute-import-and-runtime-binding-resolution-unavailable")
    );
    assert_bindings(
        source,
        &[
            ("plain = outer", "outer", Some("outer = 1")),
            ("= value;", "value", Some("value = value")),
            ("map (x", "map", Some("map; in")),
            ("object.member", "object", None),
        ],
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn nix_static_quoted_keys_and_inherited_names_retain_exact_written_definitions() {
    let source = "{ inherit \"two words\"; inherit (builtins) map foldl'; \"\" = 1; a /* path trivia */ . b = 2; ${key} = value: value; }";
    let result = output(source);
    let computed = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "<computed-key>")
        .unwrap();
    assert_eq!(computed.kind, EntityKind::Function);
    assert!(computed.flags.contains(&EntityFlag::Synthetic));
    for name in [
        "\"two words\"",
        "map",
        "foldl'",
        "\"\"",
        "a /* path trivia */ . b",
        "value",
    ] {
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "{name}: {:?}",
            result.document().entities
        );
    }
}
