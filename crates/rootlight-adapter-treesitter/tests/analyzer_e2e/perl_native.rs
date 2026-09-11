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

#[test]
fn perl_native_project_context_includes_leading_trivia() {
    use rootlight_ir::{PERL_BINDING_NAMESPACE, PerlBinding, decode_perl_binding_envelope};

    for prefix in ["\n", "\r\n\t", "\n# A standalone helper.\n"] {
        let source = format!("{prefix}use Measure (); sub consume {{ Measure::adjust(3); }}\n");
        let result = output(&source);
        let mut context = false;
        for envelope in &result.document().extensions {
            if envelope.namespace != PERL_BINDING_NAMESPACE {
                continue;
            }
            let claim = decode_perl_binding_envelope(envelope).unwrap();
            assert_eq!(claim.module().start_byte(), 0, "{prefix:?}");
            assert_eq!(
                claim.module().end_byte(),
                u64::try_from(source.len()).unwrap()
            );
            context |= claim.binding() == PerlBinding::ModuleContext;
        }
        assert!(context);
    }
}

#[test]
fn perl_native_block_eval_preserves_nested_storage_effects() {
    use rootlight_ir::{PERL_BINDING_NAMESPACE, PerlBinding, decode_perl_binding_envelope};

    for (source, dynamic, writes) in [
        ("eval { require Optional; };", 0, 0),
        ("eval { eval { require Optional; }; };", 0, 0),
        ("eval { my $text = 'eval $input'; };", 0, 0),
        ("eval { *Measure::adjust = sub { 31 }; };", 0, 1),
        ("eval { *{$name} = sub { 31 }; };", 1, 0),
        ("eval { eval $input; };", 1, 0),
        ("eval $input;", 1, 0),
        ("eval;", 1, 0),
        ("do $path;", 1, 0),
        ("eval { my $text = 'x'; $text =~ s/x/value()/e; };", 1, 0),
    ] {
        let result = output(source);
        let claims: Vec<_> = result
            .document()
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == PERL_BINDING_NAMESPACE)
            .map(|envelope| decode_perl_binding_envelope(envelope).unwrap().binding())
            .collect();
        assert!(claims.contains(&PerlBinding::ModuleContext), "{source}");
        assert_eq!(
            claims
                .iter()
                .filter(|claim| **claim == PerlBinding::DynamicWrite)
                .count(),
            dynamic,
            "{source}"
        );
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim, PerlBinding::Write { .. }))
                .count(),
            writes,
            "{source}"
        );
    }
}

#[test]
fn perl_native_bounded_project_evidence_never_claims_a_complete_context() {
    use rootlight_ir::{PERL_BINDING_NAMESPACE, PerlBinding, decode_perl_binding_envelope};
    let source = "package Harbor; sub value { 7 } *value = sub { 31 }; value();\n";
    let reference = output(source);
    for extra in 0..=2 {
        let base = limits();
        let mut ir = base.ir().clone();
        ir.max_extensions = reference.document().entities.len() + 1 + extra;
        let budget = AnalysisLimits::new(
            base.max_source_bytes(),
            base.max_syntax_nodes(),
            base.max_syntax_depth(),
            base.max_embedded_ranges(),
            base.max_reported_memory_bytes(),
            base.syntax_stream().clone(),
            base.ir_stream().clone(),
            ir,
        )
        .unwrap();
        let provider = Arc::new(provider());
        let fixture = Fixture::new(PERL, source.as_bytes());
        let result = analyze(
            &analyzer(&provider, PERL),
            &request(&fixture.snapshot, &fixture.source, PERL, &budget),
            &ExtensionSupport::default(),
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "perl-project-evidence-resource-limit")
        );
        for envelope in result
            .document()
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == PERL_BINDING_NAMESPACE)
        {
            assert_ne!(
                decode_perl_binding_envelope(envelope).unwrap().binding(),
                PerlBinding::ModuleContext
            );
        }
    }
}

#[test]
fn perl_native_project_evidence_retains_storage_and_exact_sources() {
    use rootlight_ir::{
        PERL_BINDING_NAMESPACE, PerlBinding, PerlCallableStorage, decode_perl_binding_envelope,
    };
    let client = include_str!("../../../../tests/fixtures/perl-bindings/cross-file/client.pl");
    let module =
        include_str!("../../../../tests/fixtures/perl-bindings/cross-file/module/Measure.pm");
    let storage = PerlCallableStorage {
        package: content_hash(b"Measure"),
        name: content_hash(b"adjust"),
    };
    for (source, is_client) in [(client, true), (module, false)] {
        let result = output(source);
        let document = result.document();
        let claims: Vec<_> = document
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == PERL_BINDING_NAMESPACE)
            .map(|envelope| (envelope, decode_perl_binding_envelope(envelope).unwrap()))
            .collect();
        assert_eq!(
            claims
                .iter()
                .filter(|(_, claim)| claim.binding() == PerlBinding::ModuleContext)
                .count(),
            1
        );
        if is_client {
            let (envelope, _) = claims
                .iter()
                .find(|(_, claim)| claim.binding() == PerlBinding::Call { storage })
                .unwrap();
            let span = envelope.evidence.source.as_ref().unwrap().span();
            assert_eq!(
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
                "Measure::adjust"
            );
            assert!(claims.iter().any(|(_, claim)| claim.binding()
                == PerlBinding::ModuleLoad {
                    package: storage.package
                }));
            assert!(!claims.iter().any(|(_, claim)| matches!(claim.binding(), PerlBinding::Definition { storage: found, .. } if found == storage)));
        } else {
            let (_, definition) = claims.iter().find(|(_, claim)| matches!(claim.binding(), PerlBinding::Definition { storage: found, .. } if found == storage)).unwrap();
            let PerlBinding::Definition { symbol, .. } = definition.binding() else {
                unreachable!()
            };
            assert!(
                document
                    .entities
                    .iter()
                    .any(|entity| entity.id == symbol && entity.canonical_name == "adjust")
            );
        }
        let chunk = rootlight_ir::CanonicalNormalizedFileChunk::new(
            document,
            limits().ir(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        let rebound = chunk
            .rebind(
                GenerationId::from_bytes([88; 20]),
                limits().ir(),
                &ExtensionSupport::default(),
            )
            .unwrap();
        assert_eq!(
            chunk.digest(),
            rootlight_ir::CanonicalNormalizedFileChunk::new(
                &rebound,
                limits().ir(),
                &ExtensionSupport::default()
            )
            .unwrap()
            .digest()
        );
    }
}

#[test]
fn perl_native_external_calls_preserve_invocation_and_load_evidence() {
    let source = include_str!("../../../../tests/fixtures/perl-bindings/cross-file/client.pl");
    let result = output(source);
    let document = result.document();
    let caller = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "consume")
        .unwrap();
    let call = document
        .occurrences
        .iter()
        .find(|site| site.syntactic_text_hash == content_hash(b"Measure::adjust"))
        .unwrap();
    assert_eq!(call.role, OccurrenceRole::CallSite);
    assert_eq!(call.enclosing, Some(caller.id));
    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.evidence.source.as_ref() != Some(&call.source))
    );
    let module = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntactic_text_hash == content_hash(b"Measure")
                && site.syntax_kind == "perl.use_module_name.reference"
        })
        .unwrap();
    assert_eq!(module.role, OccurrenceRole::ImportUse);
    assert!(matches!(module.target, OccurrenceTarget::Unresolved { .. }));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.source == call.source && gap.detail == "perl-function-target-unavailable"
    }));
}

#[test]
fn perl_native_external_calls_do_not_promote_bare_terms_or_code_values() {
    let source = "use Loaded (); no Disabled; require Runtime;\nsub caller { external(); &Other::value; my $code = \\&Other::value; bare_term; }\n";
    let result = output(source);
    let document = result.document();
    let mut sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind.ends_with("function_name.reference")
                && [b"external".as_slice(), b"&Other::value", b"bare_term"]
                    .iter()
                    .any(|name| site.syntactic_text_hash == content_hash(name))
        })
        .collect();
    sites.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(sites.len(), 4);
    for (site, role) in sites.iter().zip([
        OccurrenceRole::CallSite,
        OccurrenceRole::CallSite,
        OccurrenceRole::Reference,
        OccurrenceRole::Reference,
    ]) {
        assert_eq!(site.role, role);
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        assert!(
            document
                .relations
                .iter()
                .all(|edge| edge.evidence.source.as_ref() != Some(&site.source))
        );
    }
    let imports: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::ImportUse)
        .collect();
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].syntactic_text_hash, content_hash(b"Loaded"));
    assert_eq!(imports[0].syntax_kind, "perl.use_module_name.reference");
}

#[test]
fn perl_native_external_call_evidence_replays_with_fresh_generation() {
    assert_perl_artifact_replay(include_str!(
        "../../../../tests/fixtures/perl-bindings/cross-file/client.pl"
    ));
}

#[test]
fn perl_native_glob_writes_do_not_resolve_replaced_callable_bodies() {
    let source = "package Measure; sub adjust { 7 } sub stable { 19 }\n*adjust = sub { 31 }; adjust(); stable();\n";
    assert_function_calls(source, &[("adjust", None), ("stable", Some(1))]);
    let result = output(source);
    let document = result.document();
    let call = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::CallSite
                && site.syntactic_text_hash == content_hash(b"adjust")
        })
        .unwrap();
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.source == call.source && gap.detail == "perl-function-target-unavailable"
    }));
    assert!(
        document
            .relations
            .iter()
            .all(|relation| { relation.subject != RelationEndpoint::Occurrence(call.id) })
    );
}

#[test]
fn perl_native_glob_mutations_follow_package_storage_not_leaf_spelling() {
    for written in [
        "*Measure::adjust",
        "*main::Measure::adjust",
        "*{Measure::adjust}",
    ] {
        let source = format!(
            "package Measure; sub adjust {{ 7 }} package main; sub adjust {{ 19 }}\n{written} = sub {{ 31 }}; Measure::adjust(); adjust();\n"
        );
        assert_function_calls(&source, &[("Measure::adjust", None), ("adjust", Some(1))]);
    }
}

#[test]
fn perl_native_grouped_local_and_conditional_glob_writes_are_barriers() {
    for mutation in [
        "(*adjust) = sub { 31 };",
        "(*adjust, *other) = (sub { 31 }, sub { 41 });",
        "{ local *adjust; }",
        "{ local *adjust = sub { 31 }; }",
        "if ($condition) { *adjust = sub { 31 }; }",
        "sub replace { *adjust = sub { 31 }; }",
    ] {
        let source = format!("package Measure; sub adjust {{ 7 }} {mutation} adjust();\n");
        assert_function_calls(&source, &[("adjust", None)]);
    }
}

#[test]
fn perl_native_dynamic_glob_writes_preserve_lexical_callable_identity() {
    for target in ["*$name", "*{$name}", "*{'Measure::' . $name}"] {
        let source = format!(
            "package Measure; sub adjust {{ 7 }} my sub local_value {{ 19 }}\n{target} = sub {{ 31 }}; adjust(); local_value();\n"
        );
        assert_function_calls(&source, &[("adjust", None), ("local_value", Some(1))]);
    }
}

#[test]
fn perl_native_glob_reads_and_quoted_writes_do_not_replace_code_storage() {
    for expression in [
        "my $glob = *adjust;",
        "my $glob = \\*adjust;",
        "my $code = *adjust{CODE};",
        "my $text = '*adjust = sub { 31 }';",
        "# *adjust = sub { 31 };\n",
    ] {
        let source = format!("package Measure; sub adjust {{ 7 }} {expression} adjust();\n");
        assert_function_calls(&source, &[("adjust", Some(0))]);
    }
}

#[test]
fn perl_native_glob_write_barriers_replay_from_artifacts() {
    assert_perl_artifact_replay(
        "package Measure; sub adjust { 7 } *adjust = sub { 31 }; adjust();\n",
    );
}

#[test]
fn perl_native_replaced_slots_keep_proven_call_and_code_reference_roles() {
    let source = "sub adjust { 7 } *adjust = sub { 31 }; adjust; &adjust; my $code = \\&adjust;\n";
    let result = output(source);
    let document = result.document();
    let mut sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind.ends_with("function_name.reference"))
        .collect();
    sites.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(sites.len(), 3);
    for (site, role) in sites.iter().zip([
        OccurrenceRole::CallSite,
        OccurrenceRole::CallSite,
        OccurrenceRole::Reference,
    ]) {
        assert_eq!(site.role, role);
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    }
}

fn assert_function_calls(source: &str, expected: &[(&str, Option<usize>)]) {
    let sites: Vec<_> = expected
        .iter()
        .map(|&(name, target)| (name, target, OccurrenceRole::CallSite))
        .collect();
    assert_function_sites(source, &sites);
}

fn assert_function_sites(source: &str, expected: &[(&str, Option<usize>, OccurrenceRole)]) {
    let result = output(source);
    let document = result.document();
    let mut definitions: Vec<_> = document.occurrences.iter().filter(|site| {
        site.role == OccurrenceRole::Definition && matches!(site.target,
            OccurrenceTarget::Resolved { symbol } if document.entities.iter().any(|entity| entity.id == symbol && entity.kind == EntityKind::Function))
    }).collect();
    definitions.sort_by_key(|site| site.source.span().start_byte());
    let written = |site: &rootlight_ir::OccurrenceRecord| {
        let span = site.source.span();
        source
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap()
    };
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.role,
                OccurrenceRole::Reference | OccurrenceRole::CallSite
            ) && site.syntax_kind.ends_with("function_name.reference")
                && expected.iter().any(|(name, _, _)| *name == written(site))
        })
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), expected.len(), "{calls:#?}");
    for (site, &(name, target, role)) in calls.into_iter().zip(expected) {
        assert_eq!(written(site), name);
        assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
        if let Some(target) = target {
            assert_eq!(
                site.target,
                definitions[target].target,
                "{name} at {}",
                site.source.span().start_byte()
            );
            assert_eq!(site.role, role);
            let predicate = if role == OccurrenceRole::CallSite {
                RelationPredicate::Calls
            } else {
                RelationPredicate::RefersTo
            };
            let edges: Vec<_> = document
                .relations
                .iter()
                .filter(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(site.id)
                        && edge.predicate == predicate
                })
                .collect();
            assert_eq!(edges.len(), 1);
            let OccurrenceTarget::Resolved { symbol } = definitions[target].target else {
                panic!("definition is unresolved")
            };
            assert_eq!(edges[0].object, RelationEndpoint::Entity(symbol));
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
            assert!(!document.relations.iter().any(|edge| edge.subject
                == RelationEndpoint::Occurrence(site.id)
                && edge.predicate
                    == if predicate == RelationPredicate::Calls {
                        RelationPredicate::RefersTo
                    } else {
                        RelationPredicate::Calls
                    }));
        } else {
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        }
    }
}

#[test]
fn perl_native_root_package_aliases_share_namespaces_variables_and_calls() {
    let source =
        include_str!("../../../../tests/fixtures/perl-bindings/qualified_package_aliases.pl");
    assert_function_calls(
        source,
        &[
            ("value", Some(0)),
            ("::Cove::value", Some(0)),
            ("Cove::other", Some(1)),
            ("main::Cove::other", Some(1)),
            ("::Cove::other", Some(1)),
        ],
    );
    for name in [
        "$value",
        "$Cove::value",
        "$main::Cove::value",
        "$::Cove::value",
    ] {
        assert_named_binding_targets(source, name, "$value", &[Some(0)]);
    }
    let result = output(source);
    let namespaces: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Namespace)
        .collect();
    assert_eq!(namespaces.len(), 2);
    assert!(
        namespaces
            .iter()
            .any(|entity| entity.canonical_name == "Cove")
    );
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        2
    );
}

#[test]
fn perl_native_root_package_aliases_preserve_non_root_main_components() {
    let source =
        include_str!("../../../../tests/fixtures/perl-bindings/qualified_repeated_root_aliases.pl");
    assert_function_calls(
        source,
        &[
            ("main::main::Cove::value", Some(0)),
            ("::main::Cove::value", Some(0)),
            ("Cove::main::value", Some(1)),
        ],
    );
    assert_named_binding_targets(source, "$main::main::Cove::value", "$value", &[Some(0)]);
    let result = output(source);
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        2
    );
}

#[test]
fn perl_native_root_package_aliases_do_not_erase_empty_stash_components() {
    let source =
        include_str!("../../../../tests/fixtures/perl-bindings/qualified_empty_components.pl");
    assert_function_calls(
        source,
        &[
            ("::::Cove::value", None),
            ("main::::Cove::value", None),
            ("Cove::value", Some(0)),
        ],
    );
}

#[test]
fn perl_native_root_package_aliases_keep_definition_sources_and_replay() {
    let source =
        include_str!("../../../../tests/fixtures/perl-bindings/qualified_package_aliases.pl");
    let result = output(source);
    let mut sites: Vec<_> = result.document().occurrences.iter().filter(|site| {
        site.role == OccurrenceRole::Definition && matches!(site.target,
            OccurrenceTarget::Resolved { symbol } if result.document().entities.iter().any(|entity| entity.id == symbol && entity.kind == EntityKind::Namespace))
    }).collect();
    sites.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(sites.len(), 3);
    assert_eq!(sites[0].target, sites[1].target);
    assert_ne!(sites[0].target, sites[2].target);
    for (site, name) in sites.iter().zip(["Cove", "main::Cove", "main"]) {
        assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
        let span = site.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name)
        );
    }
    assert_perl_artifact_replay(source);
}

#[test]
fn perl_native_function_calls_qualified_declarations_select_written_storage() {
    let source = include_str!("../../../../tests/fixtures/perl-bindings/qualified_declaration.pl");
    assert_function_calls(source, &[("Cove::value", Some(0))]);
    let result = output(source);
    let function = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function)
        .unwrap();
    let signatures: Vec<_> = result
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .filter_map(|extension| {
            let evidence = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
            (evidence.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                && evidence.subject() == rootlight_ir::FactRef::Entity(function.id))
            .then_some(evidence)
        })
        .collect();
    assert_eq!(signatures.len(), 1);
    assert_eq!(signatures[0].text(), "sub Cove::value");
}

#[test]
fn perl_native_function_calls_qualified_forward_keeps_ambient_body_package() {
    let source = include_str!("../../../../tests/fixtures/perl-bindings/qualified_forward_body.pl");
    assert_function_calls(
        source,
        &[
            ("value", Some(1)),
            ("Cove::value", Some(0)),
            ("value", Some(0)),
        ],
    );
    let result = output(source);
    let functions: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .collect();
    assert_eq!(functions.len(), 2);
    let mut definitions: Vec<_> = result.document().occurrences.iter().filter(|site| {
        site.role == OccurrenceRole::Definition
            && matches!(site.target, OccurrenceTarget::Resolved { symbol } if functions.iter().any(|entity| entity.id == symbol))
    }).collect();
    definitions.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(definitions.len(), 3);
    assert_eq!(definitions[0].target, definitions[2].target);
    assert_ne!(definitions[0].target, definitions[1].target);
    for (site, written) in definitions.iter().zip(["value", "value", "Cove::value"]) {
        assert_eq!(site.syntactic_text_hash, content_hash(written.as_bytes()));
        let span = site.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
    }
}

#[test]
fn perl_native_function_calls_qualified_domains_do_not_collide() {
    let source =
        include_str!("../../../../tests/fixtures/perl-bindings/qualified_storage_domains.pl");
    assert_function_calls(
        source,
        &[
            ("Cove::value", Some(0)),
            ("Reef::value", Some(1)),
            ("::value", Some(2)),
            ("main::value", Some(2)),
        ],
    );
    let result = output(source);
    let functions: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .collect();
    assert_eq!(functions.len(), 3);
    assert_eq!(
        functions
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    assert!(
        functions
            .iter()
            .all(|entity| entity.canonical_name == "value")
    );
    for prefix in ["Cove::value", "Reef::value"] {
        assert!(
            functions
                .iter()
                .any(|entity| entity.qualified_name == prefix)
        );
    }
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "perl-function-ownership-unavailable")
    );
}

#[test]
fn perl_native_qualified_storage_ids_survive_body_edits_and_replay() {
    let source =
        include_str!("../../../../tests/fixtures/perl-bindings/qualified_storage_domains.pl");
    let before = output(source);
    let after = output(&source.replace("{ 13 }", "{ 1300 }"));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(identities(&before), identities(&after));
    assert_perl_artifact_replay(source);
}

#[test]
fn perl_native_function_calls_bare_names_follow_lexical_declarations() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/bare_calls.pl"),
        &[("value", Some(0)), ("value", Some(1))],
    );
}

#[test]
fn perl_native_function_calls_bare_positions_exclude_autoquoted_keys() {
    let source =
        include_str!("../../../../tests/fixtures/perl-bindings/bareword_positions_bounded.pl");
    assert_function_calls(
        source,
        &[
            ("value", Some(0)),
            ("value", Some(0)),
            ("value", Some(0)),
            ("value", Some(1)),
            ("value", Some(1)),
            ("value", Some(1)),
            ("increment", Some(2)),
        ],
    );
    let result = output(source);
    for key in ["value =>", "{value}"] {
        let offset = source.find(key).unwrap() + usize::from(key.starts_with('{'));
        assert!(!result.document().occurrences.iter().any(|site| {
            site.source.span().start_byte() == u64::try_from(offset).unwrap()
                && matches!(
                    site.role,
                    OccurrenceRole::Reference | OccurrenceRole::CallSite
                )
        }));
    }
}

#[test]
fn perl_native_function_calls_bare_names_require_prior_package_evidence() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/bare_binding_order.pl"),
        &[
            ("value", None),
            ("value", Some(0)),
            ("value", None),
            ("value", Some(0)),
        ],
    );
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/bare_import_forward.pl"),
        &[("value", Some(0))],
    );
}

#[test]
fn perl_native_function_calls_bare_invocants_follow_compile_position() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/bare_invocant_order.pl"),
        &[("value", None), ("value", Some(2)), ("value", Some(2))],
    );
}

#[test]
fn perl_native_function_calls_bare_require_is_not_a_callable_use() {
    let source = include_str!("../../../../tests/fixtures/perl-bindings/bare_require_name.pl");
    let result = output(source);
    let sites: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "perl.module_name.reference")
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].syntactic_text_hash, content_hash(b"value"));
    assert!(matches!(
        sites[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(!result.document().relations.iter().any(|edge| {
        edge.subject == RelationEndpoint::Occurrence(sites[0].id)
            && edge.predicate == RelationPredicate::Calls
    }));
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.source == sites[0].source && gap.detail == "perl-import-target-unavailable"
    }));
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/bare_require_grouping.pl"),
        &[("value", Some(0)), ("value", Some(0))],
    );
}

#[test]
fn perl_native_function_calls_ampersands_and_invoked_references_share_targets() {
    assert_function_sites(
        include_str!("../../../../tests/fixtures/perl-bindings/amper_and_reference.pl"),
        &[
            ("&value", Some(1), OccurrenceRole::CallSite),
            ("&value", Some(1), OccurrenceRole::CallSite),
            ("&value", Some(1), OccurrenceRole::CallSite),
            ("&value", Some(0), OccurrenceRole::CallSite),
        ],
    );
}

#[test]
fn perl_native_variable_coderef_regexp_evaluation_invalidates_exact_values() {
    let source = include_str!("../../../../tests/fixtures/perl-bindings/coderef_regexp_eval.pl");
    let result = output(source);
    let document = result.document();
    let call = document
        .occurrences
        .iter()
        .find(|site| site.syntactic_text_hash == content_hash(b"$call->()"))
        .unwrap();
    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.source == call.source && gap.detail == "perl-function-target-unavailable"
    }));
}

#[test]
fn perl_native_variable_coderef_calls_preserve_transparent_values_and_local_lifetimes() {
    for source in [
        "sub value { 13 } my $call = ((\\&value)); my $copy = (($call)); $copy->();",
        "sub value { 13 } sub invoke { my $call = \\&value; $call->(); } invoke();",
        "sub value { 13 } my $call = \\&value; my $text = 'x'; $text =~ s/x/y/g; $call->();",
    ] {
        let written = if source.contains("$copy->()") {
            "$copy->()"
        } else {
            "$call->()"
        };
        assert_variable_coderef_calls(source, &[(written, "value")]);
    }
}

#[test]
fn perl_native_variable_coderef_calls_do_not_guess_through_effects_or_unknown_values() {
    for source in [
        "sub first { 13 } sub second { 29 } my $call = \\&first; $call = unknown(); $call->();",
        "sub first { 13 } sub second { 29 } my $call = \\&first; if ($condition) { $call = \\&second; } $call->();",
        "sub first { 13 } sub second { 29 } my $call = \\&first; 0 && ($call = \\&second); $call->();",
        "sub first { 13 } my $call = \\&first; mutate(\\$call); $call->();",
        "sub first { 13 } my $call = \\&first; mutate($call); $call->();",
        "sub first { 13 } my $call = \\&first; $call++; $call->();",
        "sub first { 13 } my $call = \\&first; eval $input; $call->();",
        "sub first { 13 } my $call = \\&first; my $result = eval $input; $call->();",
        "sub first { 13 } sub second { 29 } my $call = \\&first; eval { $call = \\&second; }; $call->();",
        "sub first { 13 } my $call = \\&first; eval { mutate(\\$call); }; $call->();",
        "sub first { 13 } my $call = \\&first; $call .= 'suffix'; $call->();",
        "sub first { 13 } state $call = \\&first; $call->();",
        "sub first { 13 } sub second { 29 } my $call = \\&first; map { $call = \\&second } (); $call->();",
        "sub first { 13 } sub second { 29 } my $call = \\&first; sort { $call = \\&second; 0 } (); $call->();",
    ] {
        let result = output(source);
        let document = result.document();
        let call = document
            .occurrences
            .iter()
            .find(|site| site.syntactic_text_hash == content_hash(b"$call->()"))
            .unwrap();
        assert!(
            matches!(call.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            !document.relations.iter().any(|edge| {
                edge.subject == RelationEndpoint::Occurrence(call.id)
                    && edge.predicate == RelationPredicate::Calls
            }),
            "{source}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.source == call.source && gap.detail == "perl-function-target-unavailable"
            }),
            "{source}"
        );
    }
}

#[test]
fn perl_native_variable_coderef_calls_in_assignment_values_keep_receiver_identity() {
    assert_variable_coderef_calls(
        "sub value { 13 } my $call = \\&value; my $result = $call->(); print $result;",
        &[("$call->()", "value")],
    );
}

#[test]
fn perl_native_variable_coderef_calls_preserve_lexical_values() {
    assert_variable_coderef_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/coderef_lexical.pl"),
        &[("$call->()", "value"), ("&$call", "value")],
    );
}

#[test]
fn perl_native_grouped_coderef_receivers_preserve_callable_identity() {
    assert_variable_coderef_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/coderef_grouped_receiver.pl"),
        &[
            ("($call)->()", "value"),
            (
                "(( # transparent receiver group\n    $call\n))->()",
                "value",
            ),
        ],
    );
}

#[test]
fn perl_native_grouped_coderef_receivers_use_only_the_native_operand() {
    for (source, written) in [
        (
            "sub value { 13 } my $call = \\&value; ($call)->($unknown);",
            "($call)->($unknown)",
        ),
        (
            "sub value { 13 } my $café = \\&value; ((# receiver λ😀\r\n$café\r\n))->();",
            "((# receiver λ😀\r\n$café\r\n))->()",
        ),
    ] {
        assert_variable_coderef_calls(source, &[(written, "value")]);
    }
    for call in [
        "($unknown)->($call)",
        "($call, $unknown)->()",
        "($unknown || $call)->()",
        "([$call])->()",
        "('prefix' . $call)->()",
        "(($call)->(",
    ] {
        let source = format!("sub value {{ 13 }} my $call = \\&value; {call};");
        let result = output(&source);
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls),
            "{source}"
        );
    }
}

#[test]
fn perl_native_grouped_coderef_artifacts_preserve_generation_and_sources() {
    assert_perl_artifact_replay(include_str!(
        "../../../../tests/fixtures/perl-bindings/coderef_grouped_receiver.pl"
    ));
}

#[test]
fn perl_native_variable_coderef_calls_copy_values_before_reassignment() {
    assert_variable_coderef_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/coderef_copy_mutation.pl"),
        &[("$copy->()", "first"), ("$call->()", "second")],
    );
}

#[test]
fn perl_native_variable_coderef_calls_keep_shadowed_storage_distinct() {
    assert_variable_coderef_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/coderef_shadow.pl"),
        &[("$call->()", "second"), ("$call->()", "first")],
    );
}

#[test]
fn perl_native_variable_coderef_captured_mutation_has_no_single_exact_target() {
    let source =
        include_str!("../../../../tests/fixtures/perl-bindings/coderef_captured_mutation.pl");
    let result = output(source);
    let document = result.document();
    let call = document
        .occurrences
        .iter()
        .find(|site| site.syntactic_text_hash == content_hash(b"$call->()"))
        .unwrap();
    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
    assert!(!document.relations.iter().any(|edge| {
        edge.subject == RelationEndpoint::Occurrence(call.id)
            && edge.predicate == RelationPredicate::Calls
    }));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.source == call.source && gap.detail == "perl-function-target-unavailable"
    }));
}

fn assert_variable_coderef_calls(source: &str, expected: &[(&str, &str)]) {
    let result = output(source);
    let document = result.document();
    let mut sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            expected
                .iter()
                .any(|(written, _)| site.syntactic_text_hash == content_hash(written.as_bytes()))
        })
        .collect();
    sites.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(sites.len(), expected.len());
    for (site, &(written, name)) in sites.iter().zip(expected) {
        let target = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == name)
            .unwrap();
        assert_eq!(site.syntactic_text_hash, content_hash(written.as_bytes()));
        assert_eq!(site.role, OccurrenceRole::CallSite, "{written}");
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{written}"
        );
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.predicate == RelationPredicate::Calls
            })
            .collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].object, RelationEndpoint::Entity(target.id));
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
        let span = site.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
    }
}

#[test]
fn perl_native_direct_coderef_invocations_use_exact_callable_targets() {
    let source = include_str!("../../../../tests/fixtures/perl-bindings/coderef_direct_bounded.pl");
    assert_function_calls(source, &[("&value", Some(0)), ("&value", Some(0))]);
    let result = output(source);
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|site| { site.syntax_kind == "perl.coderef_application.reference" })
    );
}

#[test]
fn perl_native_direct_coderef_preserves_argument_reference_roles() {
    assert_function_sites(
        "sub value { 13 } sub other { 29 } (\\&value)->(\\&other); my $saved = \\&value;",
        &[
            ("&value", Some(0), OccurrenceRole::CallSite),
            ("&other", Some(1), OccurrenceRole::Reference),
            ("&value", Some(0), OccurrenceRole::Reference),
        ],
    );
    assert_function_calls(
        "sub value { 13 } (\\&value # grouping trivia\n)->();",
        &[("&value", Some(0))],
    );
}

#[test]
fn perl_native_direct_coderef_uses_lexical_and_qualified_storage() {
    assert_function_calls(
        "package Harbor; sub value { 13 } package main; (\\&Harbor::value)->();",
        &[("&Harbor::value", Some(0))],
    );
    assert_function_calls(
        "sub value { 13 } { my sub value { 29 } (\\&value)->(); } (\\&value)->();",
        &[("&value", Some(1)), ("&value", Some(0))],
    );
    let source = "(\\&missing)->();";
    assert_function_calls(source, &[("&missing", None)]);
    let result = output(source);
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "perl-function-target-unavailable" })
    );
}

#[test]
fn perl_native_direct_coderef_does_not_search_arbitrary_callee_descendants() {
    let source =
        "sub value { 13 } sub other { 29 } my $pick = 1; ($pick ? \\&value : \\&other)->();";
    assert_function_sites(
        source,
        &[
            ("&value", Some(0), OccurrenceRole::Reference),
            ("&other", Some(1), OccurrenceRole::Reference),
        ],
    );
    let result = output(source);
    let invocation = result
        .document()
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "perl.coderef_application.reference")
        .unwrap();
    assert!(matches!(
        invocation.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    for source in [
        "sub value { 13 } (\\&value)->(",
        "sub value { 13 } (\\&value, 1)->();",
    ] {
        let result = output(source);
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
    }
}

#[test]
fn perl_native_function_calls_ampersands_separate_proven_and_unproven_values() {
    let source =
        "sub value { 13 } &value(); &$value(); &{\"value\"}(); my $ref = \\&value; $ref->();\n";
    assert_function_sites(
        source,
        &[
            ("&value", Some(0), OccurrenceRole::CallSite),
            ("&$value", None, OccurrenceRole::CallSite),
            ("&{\"value\"}", None, OccurrenceRole::CallSite),
            ("&value", Some(0), OccurrenceRole::Reference),
        ],
    );
    let result = output(source);
    let document = result.document();
    let invocation = document
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "perl.coderef_application.reference")
        .unwrap();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "value")
        .unwrap();
    assert_eq!(
        invocation.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert_eq!(invocation.role, OccurrenceRole::CallSite);
    assert_eq!(invocation.syntactic_text_hash, content_hash(b"$ref->()"));
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.detail == "perl-function-target-unavailable" && gap.source == invocation.source
    }));
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.subject == RelationEndpoint::Occurrence(invocation.id)
                && edge.predicate == RelationPredicate::Calls
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].object, RelationEndpoint::Entity(target.id));
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&invocation.source));
}

#[test]
fn perl_native_function_calls_grouped_imports() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/grouped_subs_import.pl"),
        &[("length", Some(0)), ("reverse", Some(1))],
    );
    for prefix in [
        "use subs ((('length')), ('reverse'));",
        "use subs (qw(length reverse));",
        "use subs ('length', (qw(reverse)));",
    ] {
        let source = format!(
            "{prefix} sub length {{ 29 }} sub reverse {{ 41 }} length('abc'); reverse 'abc';\n"
        );
        assert_function_calls(&source, &[("length", Some(0)), ("reverse", Some(1))]);
    }
}

#[test]
fn perl_native_function_calls_grouped_imports_reject_expression_boundaries() {
    for prefix in [
        "no subs ('length', 'reverse');",
        "use Other ('length', 'reverse');",
        "use subs (['length', 'reverse']);",
        "use subs (('length' . 'extra'), ('reverse' . 'extra'));",
        "use subs ($condition ? 'length' : 'reverse');",
        "use subs (helper('length', 'reverse'));",
        "use subs (qw(leng\\th rev\\erse));",
        "use subs (\"$length\", \"$reverse\");",
    ] {
        let source = format!(
            "{prefix} sub length {{ 29 }} sub reverse {{ 41 }} length('abc'); reverse 'abc';\n"
        );
        assert_function_calls(&source, &[("length", None), ("reverse", None)]);
    }
    assert_function_calls(
        "sub length { 29 } length('abc'); use subs ('length'); length('abc');\n",
        &[("length", None), ("length", Some(0))],
    );
}

#[test]
fn perl_native_function_calls_imported_word_list() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/builtin_import_list.pl"),
        &[("length", Some(0))],
    );
}

#[test]
fn perl_native_function_calls_imported_list_operator() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/builtin_list_operator.pl"),
        &[("reverse", Some(0))],
    );
}

#[test]
fn perl_native_function_calls_imported_lists_reject_unproven_names() {
    for prefix in [
        "",
        "no subs qw(length reverse);",
        "use Other qw(length reverse);",
        "use subs 'length reverse';",
        "use subs 'reverse' . 'extra';",
        "use subs qw(rev\\erse);",
        "use subs $names;",
    ] {
        let source = format!(
            "{prefix} sub length {{ 29 }} sub reverse {{ 41 }} length('abc'); reverse('abc'); reverse 'abc'; CORE::reverse('abc');\n"
        );
        assert_function_calls(
            &source,
            &[
                ("length", None),
                ("reverse", None),
                ("reverse", None),
                ("CORE::reverse", None),
            ],
        );
    }
    assert_function_calls(
        "use subs qw( length\treverse\n); sub length { 29 } sub reverse { 41 } length('abc'); reverse('abc'); reverse 'abc'; CORE::reverse('abc');\n",
        &[
            ("length", Some(0)),
            ("reverse", Some(1)),
            ("reverse", Some(1)),
            ("CORE::reverse", None),
        ],
    );
    assert_function_calls(
        "package North; use subs qw(reverse); sub reverse { 29 } package South; sub reverse { 41 } reverse('abc'); North::reverse('abc');\n",
        &[("reverse", None), ("North::reverse", Some(0))],
    );
}

#[test]
fn perl_native_function_calls_follow_package_context() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/package_functions.pl"),
        &[
            ("value", Some(0)),
            ("value", Some(1)),
            ("Harbor::value", Some(0)),
            ("Cove::value", Some(1)),
        ],
    );
}

#[test]
fn perl_native_function_calls_require_exact_builtin_import_evidence() {
    for prefix in [
        "",
        "no subs 'length';",
        "use Other 'length';",
        "use subs 'length extra';",
    ] {
        let source = format!("{prefix} sub length {{ 29 }} length('abc'); CORE::length('abc');\n");
        assert_function_calls(&source, &[("length", None), ("CORE::length", None)]);
    }
    assert_function_calls(
        "sub length { 29 } length('abc'); use subs 'length'; length('abc');\n",
        &[("length", None), ("length", Some(0))],
    );
    assert_function_calls(
        "package North; use subs 'length'; sub length { 29 } package South; sub length { 41 } length('abc'); North::length('abc');\n",
        &[("length", None), ("North::length", Some(0))],
    );
}

#[test]
fn perl_native_function_calls_lexical_builtin_without_import() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/builtin_lexical.pl"),
        &[("length", Some(0)), ("length", None)],
    );
}

#[test]
fn perl_native_function_calls_lexical_builtin_over_import() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/builtin_lexical_after_import.pl"),
        &[("length", Some(1)), ("length", Some(0))],
    );
}

#[test]
fn perl_native_function_calls_preserve_lexical_body_visibility() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/lexical_sub_shadow.pl"),
        &[
            ("value", Some(0)),
            ("value", Some(0)),
            ("value", Some(1)),
            ("value", Some(0)),
        ],
    );
}

#[test]
fn perl_native_function_calls_fill_lexical_forward_declarations() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/forward_lexical_sub.pl"),
        &[("value", Some(1)), ("North::value", Some(0))],
    );
}

#[test]
fn perl_native_function_calls_keep_our_aliases_across_packages() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/our_sub_switch.pl"),
        &[
            ("value", Some(2)),
            ("value", Some(1)),
            ("South::value", Some(0)),
            ("value", Some(2)),
            ("North::value", Some(1)),
        ],
    );
}

#[test]
fn perl_native_function_calls_fill_our_forward_storage() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/our_forward_switch.pl"),
        &[
            ("value", Some(1)),
            ("North::value", Some(0)),
            ("South::value", Some(1)),
        ],
    );
}

#[test]
fn perl_native_function_calls_keep_previous_lexical_target_in_our_body() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/our_body_shadow.pl"),
        &[
            ("value", Some(0)),
            ("value", Some(1)),
            ("value", Some(0)),
            ("South::value", Some(1)),
        ],
    );
}

#[test]
fn perl_native_function_calls_distinguish_imported_builtin_overrides() {
    assert_function_calls(
        include_str!("../../../../tests/fixtures/perl-bindings/imported_builtin.pl"),
        &[
            ("length", Some(0)),
            ("CORE::length", None),
            ("length", None),
            ("North::length", Some(0)),
        ],
    );
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
        .filter(|fact| {
            fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature
                && fact.syntax_kind().as_str().ends_with(".signature")
        })
        .map(|fact| {
            let span = fact.span();
            &PERL.source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()]
        })
        .collect();
    assert_eq!(headers, ["sub adjust ($value)"]);
    let values: Vec<_> = result
        .facts()
        .iter()
        .filter(|fact| {
            fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature
                && fact.syntax_kind().as_str().ends_with(".expression")
        })
        .map(|fact| {
            let span = fact.span();
            (
                fact.syntax_kind().as_str(),
                &PERL.source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
            )
        })
        .collect();
    assert_eq!(
        values,
        [
            ("perl.lexical_code_target.expression", "my $result"),
            ("perl.unknown_code_value.expression", "$value + 1")
        ]
    );
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
    assert_function_calls(source, &[("helper", Some(0))]);
    for kind in [
        "perl.use_module_name.reference",
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
    let unknown = output("external();\n");
    let gap = unknown
        .document()
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "perl-function-target-unavailable")
        .unwrap();
    assert_eq!(gap.domain, FactDomain::Relations);
    assert_eq!(gap.source.span().start_byte(), 0);
    assert_eq!(gap.source.span().end_byte(), 8);
    assert_function_calls("external();\n", &[("external", None)]);
}

#[test]
fn perl_native_function_ownership_gaps_require_unproven_declarations() {
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
    let package = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Measure")
        .unwrap();
    assert_eq!(
        function.container,
        Some(rootlight_ir::ContainerRef::Entity(package.id))
    );
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "perl-function-ownership-unavailable")
    );
    let result = output("package Measure; sub Other::adjust ($value) { return $value; }\n");
    let function = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function)
        .unwrap();
    assert_eq!(function.canonical_name, "adjust");
    assert_eq!(function.qualified_name, "Other::adjust");
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "perl-function-ownership-unavailable")
    );
    let result = output("package Measure; my sub Other::adjust ($value) { return $value; }\n");
    let document = result.document();
    let function = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function)
        .unwrap();
    let gap = document
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "perl-function-ownership-unavailable")
        .expect("qualified lexical declarations cannot become proven package storage");
    assert_eq!(gap.domain, FactDomain::Entities);
    assert_eq!(Some(&gap.source), function.evidence.source.as_ref());
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn perl_native_artifact_replay_matches_fresh_generation() {
    assert_perl_artifact_replay(PERL.source);
}

#[test]
fn perl_native_variable_coderef_artifacts_rebind_generation() {
    assert_perl_artifact_replay(include_str!(
        "../../../../tests/fixtures/perl-bindings/coderef_copy_mutation.pl"
    ));
}

#[test]
fn perl_native_block_eval_artifacts_preserve_nested_effects() {
    assert_perl_artifact_replay(
        "\n\tpackage Measure; sub adjust { 3 } eval { require Optional; }; eval { *adjust = sub { 31 }; }; eval { eval $input; };\n",
    );
}

#[test]
fn perl_native_bare_call_artifacts_rebind_generation() {
    assert_perl_artifact_replay(include_str!(
        "../../../../tests/fixtures/perl-bindings/bareword_positions_bounded.pl"
    ));
}

fn assert_perl_artifact_replay(text: &str) {
    let provider = Arc::new(provider());
    let budget = limits();
    let fixture = Fixture::new(PERL, text.as_bytes());
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
