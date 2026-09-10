//! Perl source declarations through VFS, the production parser and validated IR.
//! Unproven bindings retain source evidence and scoped semantic gaps.

use super::*;

pub(super) const PERL: LanguageCase = LanguageCase {
    name: "perl",
    path: "src/measure.pm",
    frontend: "tree-sitter-perl-2.0.0",
    source: "package Measure; sub adjust ($value) { my $result = $value + 1; return $result; }\n",
    generated: false,
    body_before: "$value + 1",
    body_after: "$value + 23",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(PERL, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, PERL),
        &request(&fixture.snapshot, &fixture.source, PERL, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn names(result: &AnalysisOutput) -> BTreeSet<(&str, EntityKind)> {
    result
        .document()
        .entities
        .iter()
        .map(|entity| (entity.canonical_name.as_str(), entity.kind))
        .collect()
}

fn assert_definitions_exact(result: &AnalysisOutput, source: &str) {
    for entity in &result.document().entities {
        if entity.kind == EntityKind::Module {
            continue;
        }
        let sites: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(sites.len(), 1, "{}", entity.canonical_name);
        let span = sites[0].source.span();
        let written = source
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        assert_eq!(written, entity.canonical_name);
        assert_eq!(
            sites[0].syntactic_text_hash,
            content_hash(written.as_bytes())
        );
    }
}

fn assert_binding_targets(source: &str, name: &str, targets: &[Option<usize>]) {
    assert_named_binding_targets(source, name, name, targets);
}

fn assert_named_binding_targets(
    source: &str,
    name: &str,
    definition_name: &str,
    targets: &[Option<usize>],
) {
    let result = output(source);
    let document = result.document();
    let mut definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && source.get(
                    usize::try_from(site.source.span().start_byte()).unwrap()
                        ..usize::try_from(site.source.span().end_byte()).unwrap(),
                ) == Some(definition_name)
        })
        .collect();
    definitions.sort_by_key(|site| site.source.span().start_byte());
    let mut references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && source.get(
                    usize::try_from(site.source.span().start_byte()).unwrap()
                        ..usize::try_from(site.source.span().end_byte()).unwrap(),
                ) == Some(name)
        })
        .collect();
    references.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(references.len(), targets.len(), "{name}: {references:#?}");
    for (site, expected) in references.into_iter().zip(targets) {
        if let Some(index) = expected {
            assert_eq!(
                site.target,
                definitions[*index].target,
                "{name} at {}",
                site.source.span().start_byte()
            );
            assert!(matches!(site.target, OccurrenceTarget::Resolved { .. }));
            assert!(
                document
                    .relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::RefersTo
                        && relation.evidence.source.as_ref() == Some(&site.source))
            );
        } else {
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{name} at {}",
                site.source.span().start_byte()
            );
        }
    }
}

#[test]
fn perl_native_lexical_initializers_and_statement_reads_use_previous_binding() {
    assert_binding_targets(
        "my $value = 11; { my $value = $value + 1; print $value; } print $value;\n",
        "$value",
        &[Some(0), Some(1), Some(0)],
    );
    assert_binding_targets(
        "my $value = 2; { my @seen = ($value, my $value = $value + 1, $value); print $value; }\n",
        "$value",
        &[Some(0), Some(0), Some(0), Some(1)],
    );
}

#[test]
fn perl_native_control_bindings_cover_bodies_but_do_not_escape() {
    assert_binding_targets(
        "my $value = 40; for my $value (1, 2) { print $value; } print $value; if (my $value = 3) { print $value; } else { print $value; } print $value;\n",
        "$value",
        &[Some(1), Some(0), Some(2), Some(2), Some(0)],
    );
    assert_binding_targets(
        "my $value = 7; for (my $value = $value; $value < 9; $value++) { print $value; } print $value;\n",
        "$value",
        &[Some(0), Some(1), Some(1), Some(1), Some(0)],
    );
}

#[test]
fn perl_native_signatures_and_closures_preserve_lexical_ownership() {
    assert_binding_targets(
        "sub adjust ($value, $step = $value + 1) { return $value + $step; }\n",
        "$value",
        &[Some(0), Some(0)],
    );
    assert_binding_targets(
        "my $value = 9; for my $value (1, 2) { my $read = sub { return $value; }; } print $value;\n",
        "$value",
        &[Some(1), Some(0)],
    );
    assert_binding_targets(
        "my $value = 3; my $read = sub ($value) { return $value; }; print $value;\n",
        "$value",
        &[Some(1), Some(0)],
    );
}

#[test]
fn perl_native_package_aliases_shadow_outer_lexical_targets() {
    assert_binding_targets(
        "my $value = 1; { our $value; print $value; } print $value;\n",
        "$value",
        &[Some(1), Some(0)],
    );
}

#[test]
fn perl_native_our_initializer_retains_previous_lexical_reads() {
    let source = include_str!("../../../../tests/fixtures/perl-bindings/our_initializer_shadow.pl");
    assert_binding_targets(
        source,
        "$value",
        &[Some(0), Some(0), Some(0), Some(1), Some(0)],
    );
    assert_named_binding_targets(source, "$Harbor::value", "$value", &[Some(1)]);
}

#[test]
fn perl_native_our_initializer_preserves_previous_alias_or_uses_immediate_alias() {
    let source = include_str!("../../../../tests/fixtures/perl-bindings/alias_initializer.pl");
    assert_binding_targets(
        source,
        "$value",
        &[Some(0), Some(0), Some(0), Some(1), Some(0)],
    );
    assert_named_binding_targets(source, "$Cove::value", "$value", &[Some(1)]);
    let source = include_str!("../../../../tests/fixtures/perl-bindings/immediate_initializer.pl");
    assert_binding_targets(source, "$value", &[Some(0), Some(0)]);
    assert_named_binding_targets(source, "$main::value", "$value", &[Some(0)]);
}

#[test]
fn perl_native_package_variables_keep_aliases_across_package_switches() {
    let source = include_str!("../../../../tests/fixtures/perl-bindings/alias_switch.pl");
    assert_binding_targets(source, "$value", &[Some(0), Some(0), Some(0)]);
    assert_named_binding_targets(source, "$Harbor::value", "$value", &[Some(0), Some(0)]);
}

#[test]
fn perl_native_package_variables_share_reopened_storage_and_keep_all_sites() {
    let source = include_str!("../../../../tests/fixtures/perl-bindings/package_reopening.pl");
    assert_binding_targets(source, "$value", &[Some(0), Some(0), Some(0)]);
    assert_named_binding_targets(source, "$Harbor::value", "$value", &[Some(0)]);
    let result = output(source);
    let document = result.document();
    for name in ["Harbor", "$value"] {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect();
        assert_eq!(entities.len(), 1, "{name}: {entities:#?}");
        let definitions: Vec<_> = document
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
        assert_eq!(definitions.len(), 2, "{name}: {definitions:#?}");
        for site in definitions {
            let span = site.source.span();
            assert_eq!(
                source.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some(name)
            );
            assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
        }
    }
}

#[test]
fn perl_native_package_variables_keep_qualified_names_and_block_restoration() {
    let source = include_str!("../../../../tests/fixtures/perl-bindings/qualified_names.pl");
    assert_binding_targets(source, "$value", &[Some(1), Some(1), Some(0)]);
    assert_named_binding_targets(source, "$Harbor::value", "$value", &[Some(0), Some(0)]);
    assert_named_binding_targets(
        source,
        "$Cove::value",
        "$value",
        &[Some(1), Some(1), Some(1)],
    );
    let result = output(source);
    let variables: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "$value")
        .collect();
    assert_eq!(variables.len(), 2);
    assert_ne!(variables[0].id, variables[1].id);
    assert!(
        variables
            .iter()
            .any(|entity| entity.qualified_name.ends_with("Harbor::$value"))
    );
    assert!(
        variables
            .iter()
            .any(|entity| entity.qualified_name.ends_with("Cove::$value"))
    );
}

#[test]
fn perl_native_preserves_declarations_parameters_and_sigil_names() {
    let source = "package Measure; sub adjust ($value, $step = $fallback, @rest) { my ($item, @item, %item) = (); return $value; }\n";
    let result = output(source);
    for expected in [
        ("Measure", EntityKind::Namespace),
        ("adjust", EntityKind::Function),
        ("$value", EntityKind::Parameter),
        ("$step", EntityKind::Parameter),
        ("@rest", EntityKind::Parameter),
        ("$item", EntityKind::Variable),
        ("@item", EntityKind::Variable),
        ("%item", EntityKind::Variable),
    ] {
        assert!(
            names(&result).contains(&expected),
            "missing {expected:?}: {:?}",
            names(&result)
        );
    }
    assert!(!names(&result).iter().any(|(name, _)| *name == "$fallback"));
    assert_definitions_exact(&result, source);
}

#[test]
fn perl_native_runtime_oracle_sources_keep_exact_lexical_targets() {
    for (source, targets) in [
        (
            include_str!("../../../../tests/fixtures/perl-bindings/lexical_initializer.pl"),
            vec![Some(0), Some(1), Some(0)],
        ),
        (
            include_str!("../../../../tests/fixtures/perl-bindings/same_statement.pl"),
            vec![Some(0), Some(0), Some(0), Some(1)],
        ),
        (
            include_str!("../../../../tests/fixtures/perl-bindings/control_scopes.pl"),
            vec![Some(1), Some(0), Some(2), Some(2), Some(0)],
        ),
        (
            include_str!("../../../../tests/fixtures/perl-bindings/signature_default.pl"),
            vec![Some(0), Some(0)],
        ),
        (
            include_str!("../../../../tests/fixtures/perl-bindings/loop_closures.pl"),
            vec![Some(1), Some(0)],
        ),
    ] {
        assert_binding_targets(source, "$value", &targets);
    }
    assert_binding_targets(
        include_str!("../../../../tests/fixtures/perl-bindings/package_alias.pl"),
        "$value",
        &[Some(0), Some(0), Some(0)],
    );
}

#[test]
fn perl_native_loop_lists_and_unicode_names_preserve_outer_bindings() {
    assert_binding_targets(
        "my $value = 2; for my $value ($value) { print $value; } print $value;\n",
        "$value",
        &[Some(0), Some(1), Some(0)],
    );
    for source in ["use utf8; my $e\u{301} = 1; { my $e\u{301} = $e\u{301} + 1; print $e\u{301}; } print $e\u{301};\n".to_owned(), "use utf8; my $e\u{301} = 1; { my $e\u{301} = $e\u{301} + 1; print $e\u{301}; } print $e\u{301};\r\n".to_owned()] {
        assert_binding_targets(&source, "$e\u{301}", &[Some(0), Some(1), Some(0)]);
    }
}

#[test]
fn perl_native_container_reads_use_the_array_or_hash_namespace() {
    let source = "my $items = 0; my @items = (1, 2); my %items = (key => 3); print $items, @items, %items, $items[0], $items{key}, @items[0,1], @items{key}, %items[0,1], %items{key}, $#items;\n";
    let result = output(source);
    let document = result.document();
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && matches!(
                    site.syntax_kind.as_str(),
                    "perl.variable_name.reference"
                        | "perl.array_container.reference"
                        | "perl.hash_container.reference"
                        | "perl.array_length.reference"
                )
        })
        .collect();
    assert_eq!(references.len(), 10);
    for site in references {
        let name = match site.syntax_kind.as_str() {
            "perl.array_container.reference" | "perl.array_length.reference" => "@items",
            "perl.hash_container.reference" => "%items",
            _ => source
                .get(
                    usize::try_from(site.source.span().start_byte()).unwrap()
                        ..usize::try_from(site.source.span().end_byte()).unwrap(),
                )
                .unwrap(),
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: entity.id },
            "{site:#?}"
        );
    }
}

#[test]
fn perl_native_classes_roles_fields_and_forward_declarations_are_retained() {
    let source = "class Counter { field $value; method read ($offset = 1) { $value + $offset } } role Named { method name; } sub forward;\n";
    let result = output(source);
    for expected in [
        ("Counter", EntityKind::Class),
        ("$value", EntityKind::Field),
        ("read", EntityKind::Method),
        ("$offset", EntityKind::Parameter),
        ("Named", EntityKind::Trait),
        ("name", EntityKind::Method),
        ("forward", EntityKind::Function),
    ] {
        assert!(
            names(&result).contains(&expected),
            "missing {expected:?}: {:?}",
            names(&result)
        );
    }
    assert_definitions_exact(&result, source);
}

#[test]
fn perl_native_grouped_variables_exclude_initializer_reads() {
    let source =
        "my ($first, ($second, $third), @rest) = ($input, @values); our %cache; state $seen = 0;\n";
    let result = output(source);
    let variables: BTreeSet<_> = names(&result)
        .into_iter()
        .filter_map(|(name, kind)| (kind == EntityKind::Variable).then_some(name))
        .collect();
    assert_eq!(
        variables,
        BTreeSet::from(["$first", "$second", "$third", "@rest", "%cache", "$seen"])
    );
    assert_definitions_exact(&result, source);
}

#[test]
fn perl_native_opaque_text_and_anonymous_slots_do_not_invent_symbols() {
    for newline in ["\n", "\r\n"] {
        let source = "# λ😀 sub phantom {}\nmy $text = q{sub fake ($arg) {}};\n=pod\npackage Hidden;\n=cut\nmy $body = <<'END';\nsub phantom ($arg) {}\nEND\nsub actual ($, @) { 1 }\n".replace('\n', newline);
        let result = output(&source);
        let declared: BTreeSet<_> = names(&result)
            .into_iter()
            .filter_map(|(name, kind)| (kind != EntityKind::Module).then_some(name))
            .collect();
        assert_eq!(declared, BTreeSet::from(["$text", "$body", "actual"]));
        assert_definitions_exact(&result, &source);
    }
}

#[test]
fn perl_native_callable_headers_exclude_bodies() {
    let fixture = Fixture::new(PERL, PERL.source.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, PERL, &budget);
    let result = rootlight_adapter_sdk::execute_parse(
        &provider(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let headers: Vec<_> = result
        .facts()
        .iter()
        .filter(|fact| fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature)
        .map(|fact| {
            let span = fact.span();
            &PERL.source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()]
        })
        .collect();
    assert_eq!(headers, ["sub adjust ($value)"]);
}

#[test]
fn perl_native_body_edits_keep_written_entity_identity() {
    let before = output(PERL.source);
    let changed = PERL.source.replace(PERL.body_before, PERL.body_after);
    let after = output(&changed);
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.canonical_name.clone(), entity.id))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&before), identities(&after));
}

#[test]
fn perl_native_unresolved_uses_keep_scoped_source_evidence() {
    let source =
        "package Measure; use Library; sub helper { 1 } our $value = helper(); $value->method();\n";
    let result = output(source);
    for detail in [
        "perl-import-target-unavailable",
        "perl-function-target-unavailable",
        "perl-method-target-unavailable",
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == detail),
            "missing {detail}: {:?}",
            result.document().skipped_regions
        );
    }
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
    for kind in [
        "perl.identifier.reference",
        "perl.function_name.reference",
        "perl.method_application.reference",
    ] {
        let sites: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == kind)
            .collect();
        assert_eq!(sites.len(), 1, "{kind}");
        assert!(matches!(
            sites[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
    assert_binding_targets(source, "$value", &[Some(0)]);
    assert_binding_targets("print $Unknown::value;\n", "$Unknown::value", &[None]);
}

#[test]
fn perl_native_function_ownership_gap_stays_scoped_after_package_resolution() {
    let result = output(PERL.source);
    let document = result.document();
    assert!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .all(|site| matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    let function = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function)
        .unwrap();
    let gap = document
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "perl-function-ownership-unavailable")
        .expect("source-backed declarations do not prove package or lexical function ownership");
    assert_eq!(gap.domain, FactDomain::Entities);
    assert_eq!(Some(&gap.source), function.evidence.source.as_ref());
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn perl_native_artifact_replay_matches_fresh_generation() {
    let provider = Arc::new(provider());
    let budget = limits();
    let fixture = Fixture::new(PERL, PERL.source.as_bytes());
    let analyzer = analyzer(&provider, PERL);
    let (_, artifact) = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, PERL, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let successor = fixture.next_generation();
    let request = request(&successor.snapshot, &successor.source, PERL, &budget);
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
            .all(|site| site.source.generation() == successor.source.generation())
    );
}

#[test]
fn perl_native_repeated_written_declarations_keep_distinct_identities() {
    let source = "package First; sub adjust { 1 } package Second; sub adjust { 2 }\n";
    let result = output(source);
    let functions: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "adjust")
        .collect();
    assert_eq!(functions.len(), 2);
    assert_ne!(functions[0].id, functions[1].id);
    assert_definitions_exact(&result, source);
}
