//! Written document ownership through the native block parser and validated IR.
//! Heading text remains authored source, not a renderer-specific fragment ID.

use super::*;

const MARKDOWN: LanguageCase = LanguageCase {
    name: "markdown",
    path: "docs/storage.md",
    frontend: "tree-sitter-md-0.5.3",
    source: "# Storage *rules*\r\n\r\nIntro.\r\n\r\n## Shared cache\r\n\r\n[Guide]: <guide.md> \"Help\"\r\n\r\nBody.\r\n\r\nOther section\r\n=============\r\nTail.\r\n",
    generated: false,
    body_before: "Body.",
    body_after: "Longer replacement body.",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, MARKDOWN),
        &request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn cobol_markdown_body(language: &str, ending: &str) -> String {
    let prefix = if language == "cobol-fixed" {
        "       "
    } else {
        ""
    };
    [
        "identification division.",
        "program-id. Example.",
        "data division.",
        "working-storage section.",
        "01 Stored pic 9.",
        "procedure division.",
        "Main.",
        "move Stored to Stored.",
        "goback.",
        "end program Example.",
    ]
    .iter()
    .map(|line| format!("{prefix}{line}{ending}"))
    .collect()
}

#[test]
fn markdown_cobol_physical_fences_preserve_profiles_host_evidence_and_replay() {
    for ending in ["\n", "\r\n"] {
        let mut source = format!("# Examples λ😀{ending}{ending}");
        let mut fences = Vec::new();
        for language in ["cobol-fixed", "cobol-free", "cobol-fixed", "cobol-free"] {
            let fence_start = source.len();
            source.push_str(&format!("~~~{language}{ending}"));
            let body_start = source.len();
            source.push_str(&cobol_markdown_body(language, ending));
            let body_end = source.len();
            source.push_str(&format!("~~~{ending}"));
            fences.push((language, fence_start..source.len(), body_start..body_end));
            source.push_str(ending);
        }
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, MARKDOWN);
        let fixture = Fixture::new(MARKDOWN, source.as_bytes());
        let budget = limits();
        let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
        let demand = provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap();
        let (result, artifact) = analyzer
            .analyze_and_capture(
                &initial,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        assert_eq!(
            artifact.required_syntax_fact_count(&deadline()).unwrap(),
            demand
        );
        let document = result.document();
        let mut identities = BTreeSet::new();
        for (language, fence, body) in &fences {
            let local = |span: SourceSpan| {
                span.start_byte() >= u64::try_from(body.start).unwrap()
                    && span.end_byte() <= u64::try_from(body.end).unwrap()
            };
            let entities: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.language == *language
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| local(source.span()))
                })
                .collect();
            assert_eq!(
                entities.len(),
                3,
                "{language}: {:?}",
                document.skipped_regions
            );
            assert_eq!(
                entities
                    .iter()
                    .map(|entity| entity.canonical_name.as_str())
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from(["Example", "Stored", "Main"])
            );
            for entity in &entities {
                assert!(
                    identities.insert(entity.id),
                    "neighboring fences have separate owners"
                );
            }
            let definitions: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Definition && local(site.source.span()))
                .collect();
            assert_eq!(definitions.len(), 3);
            for site in definitions {
                let OccurrenceTarget::Resolved { symbol } = site.target else {
                    panic!("written definition must retain its own entity");
                };
                let entity = entities.iter().find(|entity| entity.id == symbol).unwrap();
                let span = site.source.span();
                let text = &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()];
                assert_eq!(text, entity.canonical_name);
                assert_eq!(site.syntactic_text_hash, content_hash(text.as_bytes()));
                assert_eq!(site.source.content_hash(), fixture.source.content_hash());
                assert_eq!(site.source.generation(), fixture.source.generation());
                assert_eq!(site.source.span().file(), fixture.source.span().file());
                assert_eq!(
                    site.id,
                    rootlight_ir::derive_occurrence_record_id(site).unwrap()
                );
            }
            let program = entities
                .iter()
                .find(|entity| entity.canonical_name == "Example")
                .unwrap();
            let prefix = if *language == "cobol-fixed" {
                "       "
            } else {
                ""
            };
            let header = format!("identification division.{ending}{prefix}program-id. Example.");
            let header_start = body.start + prefix.len();
            let signatures: Vec<_> = document
                .extensions
                .iter()
                .filter_map(|envelope| {
                    if envelope.namespace != rootlight_ir::LEXICAL_EXTENSION_NAMESPACE {
                        return None;
                    }
                    let evidence =
                        rootlight_ir::decode_lexical_evidence_envelope(envelope).unwrap();
                    (evidence.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                        && evidence.subject() == rootlight_ir::FactRef::Entity(program.id))
                    .then_some((envelope, evidence))
                })
                .collect();
            assert_eq!(signatures.len(), 1);
            assert_eq!(signatures[0].1.text(), header);
            let header_source = signatures[0].0.evidence.source.as_ref().unwrap();
            assert_eq!(
                header_source.span().start_byte(),
                u64::try_from(header_start).unwrap()
            );
            assert_eq!(
                header_source.span().end_byte(),
                u64::try_from(header_start + header.len()).unwrap()
            );
            assert_eq!(header_source.content_hash(), fixture.source.content_hash());
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.detail == "markdown-code-semantics-unavailable"
                    && gap.domain == FactDomain::Relations
                    && gap.source.span().start_byte() == u64::try_from(fence.start).unwrap()
                    && gap.source.span().end_byte() == u64::try_from(fence.end).unwrap()
            }));
            assert!(!document.skipped_regions.iter().any(|gap| {
                gap.detail == "signature-capture-unavailable" && local(gap.source.span())
            }));
        }
        assert_eq!(identities.len(), 12);
        assert!(
            document
                .occurrences
                .iter()
                .filter(|site| site.syntax_kind.starts_with("cobol.")
                    && site.role != OccurrenceRole::Definition)
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
        assert!(document.relations.iter().all(|edge| matches!(
            edge.predicate,
            RelationPredicate::Contains
                | RelationPredicate::Declares
                | RelationPredicate::DefinesAt
        )));
        let changed = fixture.next_generation();
        let next = request(&changed.snapshot, &changed.source, MARKDOWN, &budget);
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
    }
}

#[test]
fn markdown_cobol_container_and_deindented_fences_remain_scoped_opaque_source() {
    for language in ["cobol-fixed", "cobol-free"] {
        for ending in ["\n", "\r\n"] {
            let body = cobol_markdown_body(language, ending);
            for (opening, prefix, closing) in [
                ("> ~~~", "> ", "> ~~~"),
                ("- ~~~", "  ", "  ~~~"),
                ("   ~~~", "   ", "   ~~~"),
            ] {
                let mut source = format!("# Examples{ending}{ending}{opening}{language}{ending}");
                for line in body.split_terminator(ending) {
                    source.push_str(&format!("{prefix}{line}{ending}"));
                }
                source.push_str(&format!("{closing}{ending}{ending}"));
                let healthy_start = source.len();
                source.push_str(&format!("~~~{language}{ending}{body}~~~{ending}"));
                let provider = provider();
                let fixture = Fixture::new(MARKDOWN, source.as_bytes());
                let budget = limits();
                let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
                let parsed = rootlight_adapter_sdk::execute_parse(
                    &provider,
                    &initial.to_parse_request(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
                let opaque: Vec<_> = parsed
                    .facts()
                    .iter()
                    .filter(|fact| {
                        fact.syntax_kind().as_str() == "markdown.code_block.signature"
                            && fact.span().end_byte() <= u64::try_from(healthy_start).unwrap()
                    })
                    .collect();
                assert_eq!(opaque.len(), 1, "{language}: {opening}");
                assert!(!parsed.facts().iter().any(|fact| {
                    fact.syntax_kind().as_str().starts_with("cobol.")
                        && fact.span().start_byte() < u64::try_from(healthy_start).unwrap()
                }));
                let result = output(&source);
                let document = result.document();
                let gaps: Vec<_> = document
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.detail == "markdown-embedded-analysis-unavailable"
                            && gap.source.span() == opaque[0].span()
                    })
                    .collect();
                assert_eq!(gaps.len(), 1);
                assert_eq!(gaps[0].domain, FactDomain::Entities);
                assert_eq!(gaps[0].reason, SkippedRegionReason::UnsupportedConstruct);
                assert_eq!(
                    gaps[0].source.content_hash(),
                    content_hash(source.as_bytes())
                );
                let entities: Vec<_> = document
                    .entities
                    .iter()
                    .filter(|entity| entity.language == language)
                    .collect();
                assert_eq!(entities.len(), 3);
                assert!(entities.iter().all(|entity| {
                    entity.evidence.source.as_ref().unwrap().span().start_byte()
                        >= u64::try_from(healthy_start).unwrap()
                }));
                assert_eq!(
                    document
                        .occurrences
                        .iter()
                        .filter(|site| site.role == OccurrenceRole::Definition
                            && site.syntax_kind.starts_with("cobol."))
                        .count(),
                    3
                );
            }
        }
    }
}

#[test]
fn markdown_cobol_eof_and_error_markers_keep_exact_profile_and_neighbor_sources() {
    for language in ["cobol-fixed", "cobol-free"] {
        for ending in ["\n", "\r\n"] {
            let body = cobol_markdown_body(language, ending);
            for trailing_newline in [false, true] {
                let body = if trailing_newline {
                    body.as_str()
                } else {
                    body.strip_suffix(ending).unwrap()
                };
                let source = format!("~~~{language}{ending}{body}");
                let provider = provider();
                let fixture = Fixture::new(MARKDOWN, source.as_bytes());
                let budget = limits();
                let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
                let parsed = rootlight_adapter_sdk::execute_parse(
                    &provider,
                    &initial.to_parse_request(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
                let mut host_parser = tree_sitter::Parser::new();
                host_parser
                    .set_language(&tree_sitter_md::LANGUAGE.into())
                    .unwrap();
                let host = host_parser.parse(&source, None).unwrap();
                let result = output(&source);
                assert_eq!(
                    result
                        .document()
                        .entities
                        .iter()
                        .filter(|entity| entity.language == language)
                        .count(),
                    3,
                    "{language}, ending={ending:?}, trailing_newline={trailing_newline}, host={}",
                    host.root_node().kind()
                );
                let marker = if language == "cobol-fixed" {
                    "markdown.cobol_fixed_code_parsed.signature"
                } else {
                    "markdown.cobol_free_code_parsed.signature"
                };
                let markers: Vec<_> = parsed
                    .facts()
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str() == marker)
                    .collect();
                assert_eq!(markers.len(), 1);
                assert_eq!(markers[0].span(), fixture.source.span());
                let definitions: Vec<_> = result
                    .document()
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.syntax_kind.starts_with("cobol.")
                    })
                    .collect();
                assert_eq!(definitions.len(), 3);
                assert_eq!(
                    definitions
                        .iter()
                        .map(|site| {
                            assert_eq!(site.source.content_hash(), fixture.source.content_hash());
                            assert_eq!(site.source.generation(), fixture.source.generation());
                            assert!(matches!(site.target, OccurrenceTarget::Resolved { .. }));
                            let span = site.source.span();
                            &source[usize::try_from(span.start_byte()).unwrap()
                                ..usize::try_from(span.end_byte()).unwrap()]
                        })
                        .collect::<BTreeSet<_>>(),
                    BTreeSet::from(["Example", "Stored", "Main"])
                );
                if host.root_node().is_error() {
                    assert_eq!(parsed.report().coverage().status(), CoverageStatus::Unknown);
                    assert_eq!(result.report().coverage().status(), CoverageStatus::Unknown);
                    assert!(result.document().skipped_regions.iter().any(|gap| {
                        gap.detail == "syntax-error-recovery"
                            && gap.domain == FactDomain::Diagnostics
                            && gap.reason == SkippedRegionReason::ParseError
                            && gap.source == fixture.source
                    }));
                }
                assert!(
                    !result
                        .document()
                        .skipped_regions
                        .iter()
                        .any(|gap| gap.detail == "markdown-embedded-analysis-unavailable"
                            || gap.detail == "markdown-code-parse-unavailable")
                );
            }
            let broken = body.replace("goback.", "@@@.");
            let source = format!(
                "~~~{language}{ending}{broken}~~~{ending}{ending}~~~{language}{ending}{body}~~~{ending}"
            );
            let healthy_start = source
                .find(&format!("~~~{language}{ending}{body}"))
                .unwrap();
            let provider = provider();
            let fixture = Fixture::new(MARKDOWN, source.as_bytes());
            let budget = limits();
            let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
            let parsed = rootlight_adapter_sdk::execute_parse(
                &provider,
                &initial.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
            let marker = if language == "cobol-fixed" {
                "markdown.cobol_fixed_code_error.signature"
            } else {
                "markdown.cobol_free_code_error.signature"
            };
            let errors: Vec<_> = parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == marker)
                .collect();
            assert_eq!(errors.len(), 1);
            let result = output(&source);
            let document = result.document();
            let gap = document
                .skipped_regions
                .iter()
                .find(|gap| gap.detail == "markdown-code-parse-unavailable")
                .unwrap();
            assert_eq!(gap.source.span(), errors[0].span());
            assert_eq!(gap.reason, SkippedRegionReason::ParseError);
            assert_eq!(gap.source.content_hash(), content_hash(source.as_bytes()));
            let programs: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.language == language
                        && entity.kind == EntityKind::Module
                        && entity.canonical_name == "Example"
                })
                .collect();
            assert_eq!(
                programs.len(),
                2,
                "healthy native headers survive a body error"
            );
            assert!(programs.iter().any(|entity| {
                entity.evidence.source.as_ref().unwrap().span().start_byte()
                    < u64::try_from(healthy_start).unwrap()
            }));
            assert!(programs.iter().any(|entity| {
                entity.evidence.source.as_ref().unwrap().span().start_byte()
                    >= u64::try_from(healthy_start).unwrap()
            }));
            assert!(
                !document
                    .entities
                    .iter()
                    .any(|entity| entity.language == "markdown"
                        && matches!(
                            entity.canonical_name.as_str(),
                            "Example" | "Stored" | "Main"
                        ))
            );
        }
    }
}

#[test]
fn markdown_matlab_bindings_keep_host_spans_and_separate_examples() {
    let source = "# Variables λ😀\r\n```matlab\r\nvalue = 1;\r\nvalue = value + 1;\r\n```\r\n```matlab\r\nvalue = 2;\r\nvalue = value + 2;\r\n```\r\n```matlab\r\nresult = value;\r\n```\r\n";
    let result = output(source);
    let variables: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.language == "matlab" && entity.canonical_name == "value")
        .collect();
    assert_eq!(variables.len(), 2);
    assert_ne!(variables[0].id, variables[1].id);
    for variable in variables {
        let sites: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.target
                    == OccurrenceTarget::Resolved {
                        symbol: variable.id,
                    }
            })
            .collect();
        assert_eq!(sites.len(), 3);
        for role in [
            OccurrenceRole::Definition,
            OccurrenceRole::Write,
            OccurrenceRole::Reference,
        ] {
            assert_eq!(sites.iter().filter(|site| site.role == role).count(), 1);
        }
        let definition = sites
            .iter()
            .find(|site| site.role == OccurrenceRole::Definition)
            .unwrap()
            .source
            .span()
            .start_byte();
        for site in sites {
            let span = site.source.span();
            assert_eq!(
                source.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some("value")
            );
            assert!(span.start_byte() >= definition);
            assert!(span.start_byte() - definition < 32);
        }
    }
    let offset = source.find("result = value").unwrap() + "result = ".len();
    let read = result
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == u64::try_from(offset).unwrap()
        })
        .unwrap();
    assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
}

#[test]
fn markdown_perl_bindings_keep_host_sources_and_separate_lexical_pads() {
    let source = "# Pads λ😀\r\n```perl\r\nmy $value = 1; print $value;\r\n```\r\n```perl\r\nprint $value;\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.language == "perl" && entity.canonical_name == "$value")
        .unwrap();
    let mut reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "perl.variable_name.reference")
        .collect();
    reads.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(reads.len(), 2);
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert!(matches!(
        reads[1].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    for read in reads {
        let span = read.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("$value")
        );
        assert_eq!(read.syntactic_text_hash, content_hash(b"$value"));
    }
}

#[test]
fn markdown_perl_package_storage_does_not_escape_its_example() {
    let source = "# Packages λ😀\r\n```perl\r\npackage Harbor; our $value = 1; print $Harbor::value;\r\n```\r\n```perl\r\npackage Harbor; print $Harbor::value;\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let namespaces: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.language == "perl" && entity.canonical_name == "Harbor")
        .collect();
    assert_eq!(namespaces.len(), 2);
    assert_ne!(namespaces[0].id, namespaces[1].id);
    let variable = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "$value")
        .unwrap();
    let mut reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "perl.variable_name.reference")
        .collect();
    reads.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(reads.len(), 2);
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved {
            symbol: variable.id
        }
    );
    assert!(matches!(
        reads[1].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    for read in reads {
        let span = read.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("$Harbor::value")
        );
        assert_eq!(read.syntactic_text_hash, content_hash(b"$Harbor::value"));
    }
}

#[test]
fn markdown_perl_glob_mutations_stay_inside_their_example() {
    let source = "# Storage λ😀\r\n```perl\r\npackage Harbor; sub value { 13 } *value = sub { 29 }; value();\r\n```\r\n```perl\r\npackage Harbor; sub value { 17 } value();\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::CallSite
                && site.syntax_kind == "perl.static_function_name.reference"
        })
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 2);
    assert!(matches!(
        calls[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    let OccurrenceTarget::Resolved { symbol } = calls[1].target else {
        panic!("the independent example must retain its callable target")
    };
    let target = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert!(
        target.evidence.source.as_ref().unwrap().span().start_byte()
            > calls[0].source.span().end_byte()
    );
    for call in calls {
        let span = call.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("value")
        );
    }
}

#[test]
fn markdown_perl_bare_calls_do_not_borrow_later_or_neighbor_declarations() {
    let source = "# Functions λ😀\r\n```perl\r\npackage Harbor; sub value { 13 } my $result = value;\r\n```\r\n```perl\r\npackage Harbor; my $before = value; sub value { 29 } my $after = value;\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let mut definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.syntactic_text_hash == content_hash(b"value")
        })
        .collect();
    definitions.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(definitions.len(), 2);
    assert_ne!(definitions[0].target, definitions[1].target);
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "perl.bare_function_name.reference")
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 3);
    for (site, target) in calls.into_iter().zip([Some(0), None, Some(1)]) {
        let span = site.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("value")
        );
        assert_eq!(site.syntactic_text_hash, content_hash(b"value"));
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.predicate == RelationPredicate::Calls
            })
            .collect();
        if let Some(target) = target {
            assert_eq!(site.target, definitions[target].target);
            assert_eq!(site.role, OccurrenceRole::CallSite);
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
        } else {
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert!(edges.is_empty());
        }
    }
}

#[test]
fn markdown_perl_qualified_storage_and_root_aliases_stay_inside_their_example() {
    let source = "# Functions λ😀\r\n```perl\r\nsub main::Cove::value { 13 } ::Cove::value();\r\n```\r\n```perl\r\nsub Cove::value { 29 } main::Cove::value();\r\n```\r\n```perl\r\nCove::value();\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let functions: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .collect();
    assert_eq!(functions.len(), 2);
    assert_ne!(functions[0].id, functions[1].id);
    assert!(
        functions.iter().all(
            |entity| entity.canonical_name == "value" && entity.qualified_name == "Cove::value"
        )
    );
    let mut definitions: Vec<_> = document.occurrences.iter().filter(|site| {
        site.role == OccurrenceRole::Definition && matches!(site.target,
            OccurrenceTarget::Resolved { symbol } if functions.iter().any(|entity| entity.id == symbol))
    }).collect();
    definitions.sort_by_key(|site| site.source.span().start_byte());
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "perl.static_function_name.reference")
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(definitions.len(), 2);
    assert_eq!(calls.len(), 3);
    for (site, (name, target)) in calls.iter().zip([
        ("::Cove::value", Some(0)),
        ("main::Cove::value", Some(1)),
        ("Cove::value", None),
    ]) {
        assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
        let span = site.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name)
        );
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.predicate == RelationPredicate::Calls
            })
            .collect();
        if let Some(target) = target {
            assert_eq!(site.target, definitions[target].target);
            assert_eq!(site.role, OccurrenceRole::CallSite);
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
        } else {
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert!(edges.is_empty());
        }
    }
}

#[test]
fn markdown_perl_evaluated_replacements_keep_separate_call_targets() {
    let source = "# Calls λ😀\r\n```perl\r\nsub adjust { 13 } my $text = 'x'; $text =~ s/x/adjust()/e;\r\n```\r\n```perl\r\nsub adjust { 29 } my $text = 'y'; $text =~ s/y/adjust()/e;\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let mut definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.syntactic_text_hash == content_hash(b"adjust")
        })
        .collect();
    definitions.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(definitions.len(), 2);
    assert_ne!(definitions[0].target, definitions[1].target);
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::CallSite
                && site.syntactic_text_hash == content_hash(b"adjust")
        })
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 2);
    for (call, definition) in calls.into_iter().zip(definitions) {
        assert_eq!(call.target, definition.target);
        let span = call.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("adjust")
        );
    }
}

#[test]
fn markdown_perl_direct_coderef_calls_keep_embedded_identity_and_sources() {
    let source = "# Calls λ😀\r\n```perl\r\nsub value { 13 } (\\&value)->();\r\n```\r\n```perl\r\nsub value { 29 } ((\\&value))->();\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let mut definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.syntactic_text_hash == content_hash(b"value")
        })
        .collect();
    definitions.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(definitions.len(), 2);
    assert_ne!(definitions[0].target, definitions[1].target);
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 2);
    for (call, definition) in calls.into_iter().zip(definitions) {
        assert_eq!(call.target, definition.target);
        let span = call.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("&value")
        );
        assert_eq!(call.syntactic_text_hash, content_hash(b"&value"));
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.evidence.source.as_ref() == Some(&call.source)
        }));
    }
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == "perl.coderef_application.reference")
    );
}

#[test]
fn markdown_perl_variable_coderef_values_and_effects_stay_inside_their_example() {
    let source = "# Calls λ😀\r\n```perl\r\nsub value { 13 } my $call = \\&value; my $copy = $call; $copy->();\r\n```\r\n```perl\r\nsub value { 29 } my $call = \\&value; eval '1'; $call->();\r\n```\r\n```perl\r\n$call->();\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let definition = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition
                && site.syntactic_text_hash == content_hash(b"value")
        })
        .unwrap();
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "perl.coderef_application.reference")
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 3);
    for (index, call) in calls.into_iter().enumerate() {
        let written = if index == 0 { "$copy->()" } else { "$call->()" };
        let span = call.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
        assert_eq!(call.syntactic_text_hash, content_hash(written.as_bytes()));
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(call.id)
                    && edge.predicate == RelationPredicate::Calls
            })
            .collect();
        if index == 0 {
            assert_eq!(call.target, definition.target);
            assert_eq!(call.role, OccurrenceRole::CallSite);
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&call.source));
        } else {
            assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
            assert!(edges.is_empty());
        }
    }
}

#[test]
fn markdown_perl_grouped_coderef_receivers_keep_host_offsets_and_separate_pads() {
    let source = "# Calls λ😀\r\n```perl\r\nsub value { 13 } my $call = \\&value; ($call)->();\r\n```\r\n```perl\r\nsub value { 29 } my $call = \\&value; (($call))->();\r\n```\r\n```perl\r\n($call)->();\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let mut definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.syntactic_text_hash == content_hash(b"value")
        })
        .collect();
    definitions.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(definitions.len(), 2);
    assert_ne!(definitions[0].target, definitions[1].target);
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "perl.coderef_application.reference")
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 3);
    for (index, (call, written)) in calls
        .iter()
        .zip(["($call)->()", "(($call))->()", "($call)->()"])
        .enumerate()
    {
        let span = call.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
        assert_eq!(call.syntactic_text_hash, content_hash(written.as_bytes()));
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(call.id)
                    && edge.predicate == RelationPredicate::Calls
            })
            .collect();
        if index < 2 {
            assert_eq!(call.target, definitions[index].target);
            assert_eq!(call.role, OccurrenceRole::CallSite);
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&call.source));
        } else {
            assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
            assert!(edges.is_empty());
        }
    }
}

#[test]
fn markdown_perl_function_imports_and_storage_stay_inside_their_example() {
    let source = "# Functions λ😀\r\n```perl\r\npackage Harbor; use subs qw(reverse); sub reverse { 29 } reverse('abc');\r\n```\r\n```perl\r\npackage Harbor; sub reverse { 41 } reverse('abc'); &reverse();\r\n```\r\n";
    assert_perl_function_example_isolation(source);
}

#[test]
fn markdown_perl_grouped_imports_stay_inside_their_example() {
    let source = "# Functions λ😀\r\n```perl\r\npackage Harbor; use subs ((('reverse'))); sub reverse { 29 } reverse('abc');\r\n```\r\n```perl\r\npackage Harbor; sub reverse { 41 } reverse('abc'); &reverse();\r\n```\r\n";
    assert_perl_function_example_isolation(source);
}

fn assert_perl_function_example_isolation(source: &str) {
    let result = output(source);
    let document = result.document();
    let mut definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.syntactic_text_hash == content_hash(b"reverse")
        })
        .collect();
    definitions.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(definitions.len(), 2);
    assert_ne!(definitions[0].target, definitions[1].target);
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.syntax_kind.as_str(),
                "perl.importable_function_name.reference" | "perl.amper_function_name.reference"
            )
        })
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].target, definitions[0].target);
    assert!(matches!(
        calls[1].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(calls[2].target, definitions[1].target);
    for (call, written) in calls.into_iter().zip(["reverse", "reverse", "&reverse"]) {
        let span = call.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
        assert_eq!(call.syntactic_text_hash, content_hash(written.as_bytes()));
        if let OccurrenceTarget::Resolved { symbol } = call.target {
            assert_eq!(call.role, OccurrenceRole::CallSite);
            let edges: Vec<_> = document
                .relations
                .iter()
                .filter(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(call.id)
                        && edge.predicate == RelationPredicate::Calls
                })
                .collect();
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].object, RelationEndpoint::Entity(symbol));
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&call.source));
        }
    }
}

#[test]
fn markdown_matlab_local_calls_do_not_escape_their_example() {
    let source = "# Calls λ😀\r\n```matlab\r\nfunction result = entry(value)\r\nresult = helper(value);\r\nend\r\nfunction result = helper(value)\r\nresult = value;\r\nend\r\n```\r\n```matlab\r\nresult = helper(1);\r\n```\r\n";
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
    let offset = u64::try_from(source.find("helper(1)").unwrap()).unwrap();
    let unresolved: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.source.span().start_byte() == offset)
        .collect();
    assert_eq!(unresolved.len(), 2);
    assert!(
        unresolved
            .iter()
            .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn markdown_nix_merged_sets_preserve_recursion_and_host_definition_sites() {
    let source = "# Sets λ😀\r\n```nix\r\nlet a = rec { field = 1; }; a.added = field; in a\r\n```\r\n```nix\r\nlet a = rec { field = 2; }; a.added = field; in a\r\n```\r\n";
    let result = output(source);
    let fields: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.language == "nix" && entity.canonical_name == "field")
        .collect();
    assert_eq!(fields.len(), 2);
    assert_ne!(fields[0].id, fields[1].id);
    for field in fields {
        let span = field.evidence.source.as_ref().unwrap().span();
        let reads: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|item| {
                item.role == OccurrenceRole::Reference
                    && item.target == (OccurrenceTarget::Resolved { symbol: field.id })
            })
            .collect();
        assert_eq!(reads.len(), 1);
        let read = reads[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(read.start_byte()).unwrap()
                    ..usize::try_from(read.end_byte()).unwrap()
            ),
            Some("field")
        );
        assert!(read.start_byte() > span.end_byte());
        assert!(read.start_byte() - span.end_byte() < 24);
    }
}

#[test]
fn markdown_nix_literal_keys_preserve_host_spelling_and_example_scope() {
    let source = "# Keys λ😀\r\n```nix\r\nlet ${\"name\"} = 1; in name\r\n```\r\n```nix\r\nlet ${\"name\"} = 2; in name\r\n```\r\n";
    let result = output(source);
    let names: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|item| item.language == "nix" && item.canonical_name == r#"${"name"}"#)
        .collect();
    assert_eq!(names.len(), 2);
    assert_ne!(names[0].id, names[1].id);
    for name in names {
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|item| {
                item.role == OccurrenceRole::Definition
                    && item.target == (OccurrenceTarget::Resolved { symbol: name.id })
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(r#"${"name"}"#)
        );
        let reads: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|item| {
                item.role == OccurrenceRole::Reference
                    && item.target == (OccurrenceTarget::Resolved { symbol: name.id })
            })
            .collect();
        assert_eq!(reads.len(), 1);
        let read = reads[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(read.start_byte()).unwrap()
                    ..usize::try_from(read.end_byte()).unwrap()
            ),
            Some("name")
        );
        assert!(read.start_byte() > span.end_byte());
        assert!(read.start_byte() - span.end_byte() < 16);
    }
}

#[test]
fn markdown_nix_inherit_from_fields_keep_source_and_local_example_identities() {
    let source = "# Settings λ😀\r\n```nix\r\nlet inherit ({ port = 1; }) port; in port\r\n```\r\n```nix\r\nlet inherit ({ port = 2; }) port; in port\r\n```\r\n";
    let result = output(source);
    let fields: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|item| item.language == "nix" && item.canonical_name == "port")
        .collect();
    assert_eq!(fields.len(), 4);
    for field in fields {
        let definition = field.evidence.source.as_ref().unwrap().span();
        let reads: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|item| {
                item.role == OccurrenceRole::Reference
                    && item.target == (OccurrenceTarget::Resolved { symbol: field.id })
            })
            .collect();
        assert_eq!(reads.len(), 1);
        let read = reads[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(read.start_byte()).unwrap()
                    ..usize::try_from(read.end_byte()).unwrap()
            ),
            Some("port")
        );
        assert!(read.start_byte() > definition.end_byte());
        assert!(read.start_byte() - definition.end_byte() < 24);
    }
}

#[test]
fn markdown_nix_selected_attributes_keep_each_example_and_host_source() {
    let source = "# Settings λ😀\r\n```nix\r\nlet settings = { port = 1; }; in settings.port\r\n```\r\n```nix\r\nlet settings = { port = 2; }; in settings.port\r\n```\r\n";
    let result = output(source);
    let fields: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|item| item.language == "nix" && item.canonical_name == "port")
        .collect();
    assert_eq!(fields.len(), 2);
    assert_ne!(fields[0].id, fields[1].id);
    for field in fields {
        let definition = field.evidence.source.as_ref().unwrap().span();
        let reads: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|item| {
                item.role == OccurrenceRole::Reference
                    && item.target == (OccurrenceTarget::Resolved { symbol: field.id })
            })
            .collect();
        assert_eq!(reads.len(), 1);
        let read = reads[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(read.start_byte()).unwrap()
                    ..usize::try_from(read.end_byte()).unwrap()
            ),
            Some("port")
        );
        assert!(read.start_byte() > definition.end_byte());
        assert!(read.start_byte() - definition.end_byte() < 32);
    }
}

#[test]
fn markdown_nix_implicit_path_roots_keep_host_sources_and_separate_examples() {
    let source = "# Roots λ😀\r\n```nix\r\nlet a.b = 1; a.c = 2; in a\r\n```\r\n```nix\r\nlet a.d = 3; in a\r\n```\r\n";
    let result = output(source);
    let roots: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.language == "nix" && entity.canonical_name == "a")
        .collect();
    assert_eq!(roots.len(), 2);
    assert_ne!(roots[0].id, roots[1].id);
    for root in roots {
        let evidence = root.evidence.source.as_ref().unwrap();
        assert_eq!(
            source.get(
                usize::try_from(evidence.span().start_byte()).unwrap()
                    ..usize::try_from(evidence.span().end_byte()).unwrap()
            ),
            Some("a")
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Reference
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: root.id }))
        );
    }
}

#[test]
fn markdown_nix_bindings_retain_host_coordinates_and_separate_example_owners() {
    let source = "# Examples λ😀\r\n\r\n```nix\r\nlet identity = value: value; in identity 1\r\n```\r\n\r\n```nix\r\nlet identity = value: value; in identity 2\r\n```\r\n";
    let result = output(source);
    for (name, kind) in [
        ("identity", EntityKind::Function),
        ("value", EntityKind::Parameter),
    ] {
        let entities: Vec<_> = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect();
        assert_eq!(entities.len(), 2, "{:?}", result.document().entities);
        assert_ne!(entities[0].id, entities[1].id);
        for entity in entities {
            assert_eq!(entity.language, "nix");
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
                Some(name)
            );
        }
    }
}

#[test]
fn markdown_nix_lexical_bindings_do_not_cross_example_boundaries() {
    let source = "# Examples λ😀\r\n\r\n```nix\r\nlet value = 1; in value\r\n```\r\n\r\n```nix\r\nvalue\r\n```\r\n";
    assert_markdown_nix_lexical_boundaries(source, "value");
}

#[test]
fn markdown_nix_quoted_lexical_bindings_do_not_cross_example_boundaries() {
    let source = "# Examples λ😀\r\n\r\n```nix\r\nlet \"\\value\" = 1; in value\r\n```\r\n\r\n```nix\r\nvalue\r\n```\r\n";
    assert_markdown_nix_lexical_boundaries(source, r#""\value""#);
}

fn assert_markdown_nix_lexical_boundaries(source: &str, definition: &str) {
    let result = output(source);
    let document = result.document();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == definition)
        .unwrap();
    let mut reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "nix.identifier.reference")
        .collect();
    reads.sort_by_key(|occurrence| occurrence.source.span().start_byte());
    assert_eq!(reads.len(), 2);
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert!(matches!(
        reads[1].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    for read in reads {
        let span = read.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("value")
        );
    }
}

#[test]
fn markdown_objective_c_generic_bindings_retain_host_sources_and_distinct_owners() {
    let fixture = include_str!("../../../../tests/fixtures/objective-c/generics.m");
    let source = format!(
        "# Generic types\n\n```objective-c\n{fixture}\n```\n\n```objective-c\n{fixture}\n```\n"
    );
    let result = output(&source);
    let parameters: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::TypeParameter)
        .collect();
    assert_eq!(parameters.len(), 14);
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Method && entity.canonical_name == "extra")
            .count(),
        2
    );
    assert_eq!(
        parameters
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        14
    );
    for parameter in parameters {
        assert_eq!(parameter.language, "objective-c");
        let reference = parameter.evidence.source.as_ref().unwrap();
        let span = reference.span();
        let written = source
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        assert_eq!(written, parameter.canonical_name);
        let occurrence = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        }
            })
            .unwrap();
        assert_eq!(&occurrence.source, reference);
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(written.as_bytes())
        );
    }
}

#[test]
fn markdown_swift_extension_examples_keep_distinct_stable_method_identities() {
    let fixture = "struct Entry {}\nextension Entry { func copy() -> Entry { return self } }\n";
    let source = format!("# Extensions\n\n```swift\n{fixture}\n```\n\n```swift\n{fixture}\n```\n");
    let initial = output(&source);
    let methods: Vec<_> = initial
        .document()
        .entities
        .iter()
        .filter(|entity| {
            entity.language == "swift"
                && entity.kind == EntityKind::Method
                && entity.canonical_name == "copy"
        })
        .collect();
    assert_eq!(methods.len(), 2);
    let ids: BTreeSet<_> = methods.iter().map(|entity| entity.id).collect();
    assert_eq!(ids.len(), 2);
    for method in methods {
        let reference = method.evidence.source.as_ref().unwrap();
        let span = reference.span();
        assert_eq!(
            source
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                )
                .unwrap(),
            "func copy() -> Entry { return self }"
        );
    }
    let changed = output(&source.replace("return self", "return  self"));
    assert_eq!(
        ids,
        changed
            .document()
            .entities
            .iter()
            .filter(|entity| entity.language == "swift"
                && entity.kind == EntityKind::Method
                && entity.canonical_name == "copy")
            .map(|entity| entity.id)
            .collect()
    );
}

#[test]
fn markdown_objective_c_forward_declarations_keep_host_sources_and_separate_examples() {
    let fixture = include_str!("../../../../tests/fixtures/objective-c/forwards.m");
    let source = format!(
        "# Forward types\n\n```objective-c\n{fixture}\n```\n\n```objective-c\n{fixture}\n```\n"
    );
    let result = output(&source);
    let declarations: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Declaration)
        .collect();
    assert_eq!(declarations.len(), 14);
    let mut identities = BTreeSet::new();
    for declaration in declarations {
        let OccurrenceTarget::Resolved { symbol } = declaration.target else {
            panic!("embedded forward declaration has its written identity");
        };
        identities.insert(symbol);
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.language, "objective-c");
        let span = declaration.source.span();
        let written = source
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        assert_eq!(written, entity.canonical_name);
        assert_eq!(
            declaration.syntactic_text_hash,
            content_hash(written.as_bytes())
        );
    }
    assert_eq!(identities.len(), 14);
}

#[test]
fn markdown_lua_bindings_keep_visibility_and_example_boundaries() {
    let source = "# Examples\r\n\r\n~~~lua\r\nlocal outer = 1\r\ndo\r\n  local outer = outer\r\n  local function capture(parameter)\r\n    local snapshot = outer\r\n    local recursive = capture\r\n    return parameter\r\n  end\r\nend\r\nreturn outer\r\n~~~\r\n\r\n~~~lua\r\nlocal first = outer\r\nlocal outer = 2\r\nlocal second = outer\r\nlocal assigned = function() return assigned end\r\n~~~\r\n\r\n~~~rust\r\nfn foreign() {}\r\n~~~\r\n\r\n~~~lua\r\nlocal third = outer\r\nlocal missing = foreign\r\n~~~\r\n";
    let result = assert_lua_reference_bindings_in(
        MARKDOWN,
        source,
        &[
            ("local outer = outer", "outer", Some("local outer = 1")),
            (
                "local snapshot = outer",
                "outer",
                Some("local outer = outer"),
            ),
            (
                "local recursive = capture",
                "capture",
                Some("local function capture"),
            ),
            ("return parameter", "parameter", Some("parameter)")),
            ("return outer", "outer", Some("local outer = 1")),
            ("local first = outer", "outer", None),
            ("local second = outer", "outer", Some("local outer = 2")),
            ("return assigned", "assigned", None),
            ("local third = outer", "outer", None),
            ("local missing = foreign", "foreign", None),
        ],
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "markdown-code-semantics-unavailable")
    );
    assert_eq!(result.report().coverage().status(), CoverageStatus::Bounded);
}

#[test]
fn markdown_lua_bounded_plans_keep_references_without_guessing_targets() {
    let source = format!(
        "> paragraph\n{}\n~~~lua\nlocal value = 1\nreturn value\n~~~\n",
        "> continued\n".repeat(33)
    );
    let result =
        assert_lua_reference_bindings_in(MARKDOWN, &source, &[("return value", "value", None)]);
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "markdown-inline-budget-unavailable")
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == rootlight_ir::RelationPredicate::RefersTo)
    );
}

#[test]
fn markdown_lua_binding_artifacts_rebind_exact_relations() {
    let source = "~~~lua\nlocal value = 1\nreturn value\n~~~\n\n~~~lua\nlocal value = 2\nreturn value\n~~~\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, MARKDOWN, &budget);
    let replay = analyzer
        .analyze_from_artifact(
            &next,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &next, &ExtensionSupport::default());
    assert_eq!(replay.document(), fresh.document());
    assert_eq!(replay.report(), fresh.report());
    let references: Vec<_> = first
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "lua.identifier.reference")
        .collect();
    assert_eq!(references.len(), 2);
    assert_ne!(references[0].target, references[1].target);
    for occurrence in references {
        assert!(matches!(
            occurrence.target,
            OccurrenceTarget::Resolved { .. }
        ));
        let current = replay
            .document()
            .occurrences
            .iter()
            .find(|item| {
                item.syntax_kind == occurrence.syntax_kind
                    && item.source.span() == occurrence.source.span()
            })
            .unwrap();
        assert_eq!(current.target, occurrence.target);
        assert_eq!(current.source.generation(), changed.source.generation());
    }
    let relations: Vec<_> = replay
        .document()
        .relations
        .iter()
        .filter(|relation| relation.predicate == rootlight_ir::RelationPredicate::RefersTo)
        .collect();
    assert_eq!(relations.len(), 2);
    for relation in relations {
        assert_eq!(
            relation.evidence.source.as_ref().unwrap().generation(),
            changed.source.generation()
        );
    }
}

#[test]
fn markdown_classic_basic_remains_source_bound_without_inventing_a_dialect() {
    let source =
        "~~~visual-basic-classic\nAttribute VB_Name = \"Meter\"\nSub Run()\nEnd Sub\n~~~\n";
    let result = output(source);
    let document = result.document();
    for name in ["Meter", "Run"] {
        assert!(
            document.entities.iter().any(|entity| {
                entity.language == "visual-basic-classic" && entity.canonical_name == name
            }),
            "missing {name}"
        );
    }
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.language == "vba")
    );
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "markdown-embedded-analysis-unavailable")
    );
    let gap = document
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "visual-basic-dialect-unproven")
        .unwrap();
    let start = usize::try_from(gap.source.span().start_byte()).unwrap();
    let end = usize::try_from(gap.source.span().end_byte()).unwrap();
    assert!(source[start..end].contains("Attribute VB_Name = \"Meter\""));
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn markdown_udt_fields_stay_bound_to_each_example_type() {
    let mut source = "# Types\n\n".to_owned();
    let mut examples = Vec::new();
    for language in ["vba", "visual-basic-classic"].into_iter().cycle().take(12) {
        let start = source.len();
        source.push_str(&format!("~~~{language}\nPrivate Type Coordinate\nX As Long\nEnd Type\nSub Run(ByRef point As Coordinate)\npoint.X = 1\nEnd Sub\n~~~\n\n"));
        examples.push((start, source.len(), language));
    }
    let result = output(&source);
    let mut identities = BTreeSet::new();
    for (start, end, language) in examples {
        let local = |span: SourceSpan| {
            usize::try_from(span.start_byte()).unwrap() >= start
                && usize::try_from(span.end_byte()).unwrap() <= end
        };
        let field = result
            .document()
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Field
                    && entity.canonical_name == "X"
                    && local(entity.evidence.source.as_ref().unwrap().span())
            })
            .unwrap();
        assert_eq!(field.language, language);
        identities.insert(field.id);
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.let_member.reference" && local(site.source.span())
            })
            .unwrap();
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: field.id });
        assert_eq!(site.source.content_hash(), content_hash(source.as_bytes()));
    }
    assert_eq!(identities.len(), 12);
}

#[test]
fn markdown_classic_and_vba_examples_keep_separate_languages_and_bindings() {
    let body =
        "Attribute VB_Name = \"Meter\"\nSub Run(ByVal value As Long)\nvalue = value + 1\nEnd Sub\n";
    let mut source = "# Examples λ\r\n\r\n".to_owned();
    let mut spans = Vec::new();
    for label in ["visual-basic-classic", "vba", "visual-basic-classic"]
        .into_iter()
        .cycle()
        .take(40)
    {
        let start = source.len();
        source.push_str(&format!(
            "~~~{label}\r\n{}~~~\r\n\r\n",
            body.replace('\n', "\r\n")
        ));
        spans.push((start, source.len(), label));
    }
    let result = output(&source);
    let document = result.document();
    let mut parameters = BTreeSet::new();
    for (start, end, label) in spans {
        let local = |span: SourceSpan| {
            usize::try_from(span.start_byte()).unwrap() >= start
                && usize::try_from(span.end_byte()).unwrap() <= end
        };
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == "value"
                    && local(entity.evidence.source.as_ref().unwrap().span())
            })
            .unwrap();
        assert_eq!(parameter.language, label);
        parameters.insert(parameter.id);
        let references: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && local(site.source.span())
                    && site.syntactic_text_hash == content_hash(b"value")
            })
            .collect();
        assert_eq!(references.len(), 2);
        for site in references {
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved {
                    symbol: parameter.id
                }
            );
            assert_eq!(site.source.content_hash(), content_hash(source.as_bytes()));
        }
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .filter(|gap| {
                    gap.detail == "visual-basic-dialect-unproven" && local(gap.source.span())
                })
                .count(),
            usize::from(label == "visual-basic-classic")
        );
    }
    assert_eq!(parameters.len(), 40);
}

#[test]
fn markdown_classic_class_replay_preserves_quoted_host_ranges_and_instance_targets() {
    let body = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nSub Run(ByVal target As Variant)\nSet target = Me\nEnd Sub\n";
    let source = format!("~~~visual-basic-classic\n{body}~~~\n")
        .lines()
        .map(|line| format!("> {line}\r\n"))
        .collect::<String>();
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let (result, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let document = result.document();
    let class = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Holder")
        .unwrap();
    assert_eq!(class.language, "visual-basic-classic");
    let start = u64::try_from(source.find("Me").unwrap()).unwrap();
    let instance = document
        .occurrences
        .iter()
        .find(|site| site.source.span().start_byte() == start)
        .unwrap();
    assert_eq!(
        instance.target,
        OccurrenceTarget::Resolved { symbol: class.id }
    );
    assert_eq!(instance.source.span().end_byte(), start + 2);
    assert_eq!(
        instance.source.content_hash(),
        content_hash(source.as_bytes())
    );
    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, MARKDOWN, &budget);
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
    for gap in replay
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "visual-basic-dialect-unproven")
    {
        assert_eq!(gap.source.generation(), changed.source.generation());
    }
}

#[test]
fn markdown_classic_parse_and_range_limits_keep_scoped_gaps() {
    let source =
        "~~~visual-basic-classic\nSub Broken(\n~~~\n\n~~~vba\nSub Healthy()\nEnd Sub\n~~~\n";
    let full = output(source);
    assert!(
        full.document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Healthy" && entity.language == "vba")
    );
    let gap = full
        .document()
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "markdown-code-parse-unavailable")
        .unwrap();
    assert_eq!(gap.reason, SkippedRegionReason::ParseError);
    assert!(
        usize::try_from(gap.source.span().end_byte()).unwrap()
            < source.find("Sub Healthy").unwrap()
    );
    let dialect = full
        .document()
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "visual-basic-dialect-unproven")
        .unwrap();
    assert_eq!(dialect.source.span(), gap.source.span());
    let defaults = limits();
    let budget = AnalysisLimits::new(
        defaults.max_source_bytes(),
        defaults.max_syntax_nodes(),
        defaults.max_syntax_depth(),
        0,
        defaults.max_reported_memory_bytes(),
        defaults.syntax_stream().clone(),
        defaults.ir_stream().clone(),
        defaults.ir().clone(),
    )
    .unwrap();
    let provider = Arc::new(provider());
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let limited = analyze(
        &analyzer(&provider, MARKDOWN),
        &request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget),
        &ExtensionSupport::default(),
    );
    assert!(
        !limited
            .document()
            .entities
            .iter()
            .any(|entity| matches!(entity.language.as_str(), "vba" | "visual-basic-classic"))
    );
    assert_eq!(
        limited
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "markdown-code-budget-unavailable"
                && gap.reason == SkippedRegionReason::ResourceLimit)
            .count(),
        2
    );
}

#[test]
fn markdown_classic_identity_partition_retains_language_provenance() {
    let source = format!(
        "~~~visual-basic-classic\nAttribute VB_Name = \"Meter\"\nSub Run()\n{}End Sub\n~~~\n",
        "unknown = another\n".repeat(100)
    );
    let provider = Arc::new(provider());
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let defaults = limits();
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &defaults);
    let demand = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let budget = limits_with_syntax_records(demand + 8);
    let result = analyze(
        &analyzer(&provider, MARKDOWN),
        &request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget),
        &ExtensionSupport::default(),
    );
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Run"
                && entity.language == "visual-basic-classic")
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.language == "vba")
    );
    assert!(
        result
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-extraction-limit")
    );
    assert_eq!(
        result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "visual-basic-dialect-unproven")
            .count(),
        1
    );
}

#[test]
fn markdown_code_labels_require_declared_languages_without_content_inference() {
    for (label, supported) in [
        ("rust", true),
        ("rs", true),
        ("", false),
        ("text", false),
        ("rusty", false),
        ("rust&#32;", false),
    ] {
        let source = format!("~~~{label}\nfn example() {{}}\n~~~\n");
        let result = output(&source);
        assert_eq!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Function
                    && entity.canonical_name == "example"),
            supported,
            "{label}"
        );
        assert_eq!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "markdown-embedded-analysis-unavailable"),
            !supported,
            "{label}"
        );
    }
}

#[test]
fn markdown_independent_fences_reuse_fragment_capacity_with_exact_sources() {
    let source = format!(
        "# Examples λ😀\r\n\r\n{}",
        (0..40)
            .map(|index| format!("~~~rust\r\nfn example_{index}() {{}}\r\n~~~\r\n\r\n"))
            .collect::<String>()
    );
    let result = output(&source);
    let functions: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .collect();
    assert_eq!(functions.len(), 40);
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "markdown-code-budget-unavailable" })
    );
    for index in 0..40 {
        let name = format!("example_{index}");
        let function = functions
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == OccurrenceTarget::Resolved {
                            symbol: function.id,
                        }
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name.as_str())
        );
        assert_eq!(
            definition.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(
            definition.syntactic_text_hash,
            content_hash(name.as_bytes())
        );
    }
}

#[test]
fn markdown_foreign_code_reuses_range_capacity_after_native_paragraphs() {
    let source = format!(
        "{}{}",
        "Paragraph.\n\n".repeat(31),
        (0..33)
            .map(|index| format!("~~~rust\nfn item_{index}() {{}}\n~~~\n\n"))
            .collect::<String>()
    );
    let result = output(&source);
    let names: BTreeSet<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(names.len(), 33);
    assert!(names.contains("item_0"));
    assert!(names.contains("item_32"));
    let gaps: Vec<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "markdown-code-budget-unavailable")
        .collect();
    assert!(gaps.is_empty());
}

#[test]
fn markdown_fragment_overflow_stays_scoped_to_one_fence() {
    let source = format!(
        "> ~~~rust\n> fn blocked() {{\n{}> }}\n> ~~~\n\n~~~rust\nfn healthy() {{}}\n~~~\n",
        "> let value = 1;\n".repeat(33)
    );
    let result = output(&source);
    let names: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(names, ["healthy"]);
    let gaps: Vec<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "markdown-code-budget-unavailable")
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].reason, SkippedRegionReason::ResourceLimit);
    assert_eq!(
        gaps[0].source.content_hash(),
        content_hash(source.as_bytes())
    );
    assert!(
        usize::try_from(gaps[0].source.span().end_byte()).unwrap()
            < source.find("fn healthy").unwrap()
    );
}

#[test]
fn markdown_sequential_fences_preserve_document_node_and_disabled_range_limits() {
    let source = (0..40)
        .map(|index| format!("~~~rust\nfn item_{index}() {{}}\n~~~\n\n"))
        .collect::<String>();
    let full = output(&source);
    let defaults = limits();
    let provider = Arc::new(provider());
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    for (nodes, ranges) in [
        (
            full.report().resources().syntax_nodes() - 16,
            defaults.max_embedded_ranges(),
        ),
        (defaults.max_syntax_nodes(), 0),
    ] {
        let budget = AnalysisLimits::new(
            defaults.max_source_bytes(),
            nodes,
            defaults.max_syntax_depth(),
            ranges,
            defaults.max_reported_memory_bytes(),
            defaults.syntax_stream().clone(),
            defaults.ir_stream().clone(),
            defaults.ir().clone(),
        )
        .unwrap();
        let result = analyze(
            &analyzer(&provider, MARKDOWN),
            &request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget),
            &ExtensionSupport::default(),
        );
        validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        let count = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count();
        assert!(result.report().resources().syntax_nodes() <= nodes);
        if ranges == 0 {
            assert_eq!(count, 0);
        } else {
            assert!(count > 0 && count < 40);
        }
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.detail == "markdown-code-budget-unavailable"
                && gap.reason == SkippedRegionReason::ResourceLimit
        }));
    }
}

#[test]
fn markdown_code_parse_errors_preserve_healthy_neighboring_examples() {
    let source = "~~~rust\nfn broken( {\n~~~\n\n~~~rust\nfn healthy() {}\n~~~\n";
    let result = output(source);
    assert!(result.document().entities.iter().any(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "healthy"));
    let gap = result
        .document()
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "markdown-code-parse-unavailable")
        .unwrap();
    assert_eq!(gap.reason, SkippedRegionReason::ParseError);
    let span = gap.source.span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        "~~~rust\nfn broken( {\n~~~\n"
    );
}

#[test]
fn markdown_fenced_definitions_match_standalone_language_evidence() {
    let provider = Arc::new(provider());
    let budget = limits();
    let cases: Vec<_> = CASES
        .into_iter()
        .chain([
            CSS_CASE,
            JSON_CASE,
            jsonc_native::JSONC,
            LanguageCase {
                name: "coq",
                path: "src/written.v",
                frontend: "tree-sitter-rocq-0.2.0",
                source: "Definition written := 0.\n",
                generated: false,
                body_before: "0",
                body_after: "1",
            },
            LanguageCase {
                name: "v",
                path: "src/written.v",
                frontend: "tree-sitter-v-0.0.6",
                source: "module written\nstruct Box { value int }\nfn read(value int) int { return value + 1 }\n",
                generated: false,
                body_before: "value + 1",
                body_after: "value + 2",
            },
            LanguageCase { name: "verilog", path: "src/written.v", frontend: "tree-sitter-verilog-0.4.1", source: "module Written; wire value; endmodule\n", generated: false, body_before: "wire value", body_after: "wire other" },
            LanguageCase { name: "systemverilog", path: "src/written.sv", frontend: "tree-sitter-systemverilog-0.4.1", source: "module Written; wire value; endmodule\n", generated: false, body_before: "wire value", body_after: "wire other" },
            LanguageCase {
                name: "eiffel",
                path: "src/written.e",
                frontend: "tree-sitter-eiffel-1.0.0",
                source: "class BOX\nfeature\n value: INTEGER\nend\n",
                generated: false,
                body_before: "INTEGER",
                body_after: "STRING",
            },
            LanguageCase {
                name: "julia",
                path: "src/written.jl",
                frontend: "tree-sitter-julia-0.25.0",
                source: "module Written\nfunction measure(value::Int)\n value + 1\nend\nend\n",
                generated: false,
                body_before: "value + 1",
                body_after: "value + 2",
            },
            LanguageCase {
                name: "cmake",
                path: "src/Written.cmake",
                frontend: "tree-sitter-cmake-0.7.5",
                source: "function(Written value)\nset(local ${value})\nendfunction()\n",
                generated: false,
                body_before: "${value}",
                body_after: "changed",
            },
            TOML_CASE,
            yaml_native::YAML,
            html_native::HTML,
            astro_native::ASTRO,
            sql_native::SQL,
            r_native::R,
            solidity_native::SOLIDITY,
            scala_native::SCALA,
            dart_native::DART,
            powershell_native::POWERSHELL,
            objective_c_native::OBJECTIVE_C,
            ocaml_native::OCAML,
            ocaml_native::OCAML_INTERFACE,
            nix_native::NIX,
            matlab_native::MATLAB,
            perl_native::PERL,
            groovy_native::GROOVY,
            vba_native::VBA,
            LanguageCase {
                name: "apex",
                path: "src/definitions.cls",
                frontend: "tree-sitter-sfapex-3.0.1",
                source: "public class CountBox { public Integer Add(Integer Value) { return Value+1; } }",
                generated: false,
                body_before: "Value+1",
                body_after: "Value+2",
            },
            LanguageCase {
                name: "assembly",
                path: "src/definitions.asm",
                frontend: "tree-sitter-asm-0.24.0",
                source: "Run PROC NEAR\nmov rax, 1\nRun ENDP\n",
                generated: false,
                body_before: "rax, 1",
                body_after: "rax, 2",
            },
            LanguageCase {
                name: "common-lisp",
                path: "src/definitions.lisp",
                frontend: "tree-sitter-commonlisp-0.4.1",
                source: "(defun measure (value) (+ value 1))\n",
                generated: false,
                body_before: "value 1",
                body_after: "value 2",
            },
            LanguageCase {
                name: "dtd",
                path: "src/written.dtd",
                frontend: "tree-sitter-xml-dtd-0.7.0",
                source: "<!ELEMENT root EMPTY>",
                generated: false,
                body_before: "EMPTY",
                body_after: "ANY",
            },
            LanguageCase {
                name: "fortran",
                path: "src/Example.f90",
                frontend: "tree-sitter-fortran-0.6.0",
                source: "module Geometry\n  type :: Point\n    integer :: X, Y = 2\n  end type Point\ncontains\n  function Measure(Value) result(Answer)\n    integer :: Value, Answer\n    Answer = Value\n  end function Measure\n  subroutine Store(Value)\n    integer :: Value\n    call Sink(Value)\n  end subroutine Store\nend module Geometry\n",
                generated: false,
                body_before: "Answer = Value",
                body_after: "Answer = Value + 1",
            },
            LanguageCase {
                name: "glsl",
                path: "src/written.glsl",
                frontend: "tree-sitter-glsl-0.2.0",
                source: "float Scale(float Value) { return Value + 1.0; }\n",
                generated: false,
                body_before: "Value + 1.0",
                body_after: "Value  + 2.0",
            },
            LanguageCase {
                name: "hlsl",
                path: "src/written.hlsl",
                frontend: "tree-sitter-hlsl-0.2.0",
                source: "float Scale(float Value) { return Value + 1.0; }\n",
                generated: false,
                body_before: "Value + 1.0",
                body_after: "Value  + 2.0",
            },
            LanguageCase {
                name: "pascal",
                path: "src/definitions.pas",
                frontend: "tree-sitter-pascal-0.10.2",
                source: "procedure Adjust(Value: Integer); begin Value := Value+1; end;\n",
                generated: false,
                body_before: "Value+1",
                body_after: "Value+2",
            },
            LanguageCase {
                name: "plsql",
                path: "src/Example.pkb",
                frontend: "tree-sitter-plsql-0.0.1",
                source: "create or replace package body \"Case.Name\" as\nprocedure Work$(Value# in number) as\nbegin\nnull;\nend wOrK$;\nend \"Case.Name\";\n/\n",
                generated: false,
                body_before: "null;",
                body_after: "null; null;",
            },
            LanguageCase {
                name: "sas",
                path: "src/definitions.sas",
                frontend: "tree-sitter-sas-0.4.2",
                source: "function Convert(Value); return Value+1; endsub;\n",
                generated: false,
                body_before: "Value+1",
                body_after: "Value+2",
            },
            LanguageCase {
                name: "scheme",
                path: "src/definitions.scm",
                frontend: "tree-sitter-scheme-native-local-0.24.7-1",
                source: "(define (measure value) (+ value 1))\n",
                generated: false,
                body_before: "value 1",
                body_after: "value 2",
            },
            LanguageCase {
                name: "wgsl",
                path: "src/written.wgsl",
                frontend: "tree-sitter-wgsl-0.0.9",
                source: "struct Uniforms { scale: f32, }\nalias Δ = vec2<f32>;\nvar<private> counter: i32;\n@compute @workgroup_size(1)\nfn Main() { var local: i32 = 1; counter = local; }\n",
                generated: false,
                body_before: "counter = local",
                body_after: "counter  = local",
            },
            LanguageCase {
                name: "xml",
                path: "src/written.xml",
                frontend: "tree-sitter-xml-0.7.0",
                source: "<root><item value=\"plain\"/><item value='other'>text</item></root>",
                generated: false,
                body_before: "text",
                body_after: "changed text",
            },
            LanguageCase {
                name: "haskell",
                path: "src/Example.hs",
                frontend: "tree-sitter-haskell-0.23.1",
                source: "module Example where\npick :: Int -> Int\npick 0 = 1\npick value = value\n",
                generated: false,
                body_before: "pick 0 = 1",
                body_after: "pick 0 = 2",
            },
            LanguageCase {
                name: "fsharp",
                path: "src/Example.fsx",
                frontend: "tree-sitter-fsharp-0.3.12",
                source: "module Example\nlet pick value = value + 1\n",
                generated: false,
                body_before: "value + 1",
                body_after: "value + 2",
            },
            LanguageCase {
                name: "fsharp-signature",
                path: "src/Example.fsi",
                frontend: "tree-sitter-fsharp-signature-0.3.12",
                source: "module Example\nval pick : int -> int\n",
                generated: false,
                body_before: "int -> int",
                body_after: "int -> string",
            },
            LanguageCase {
                name: "visual-basic-classic",
                ..vba_native::VBA
            },
            LanguageCase {
                name: "cobol-fixed",
                path: "src/Example.cbl",
                frontend: "tree-sitter-cobol-fixed-0.0.1",
                source: concat!(
                    "       identification division.\n",
                    "       program-id. Example.\n",
                    "       data division.\n",
                    "       working-storage section.\n",
                    "       01 Stored pic 9.\n",
                    "       procedure division.\n",
                    "       Main.\n",
                    "       display 'first'.\n",
                    "       goback.\n",
                    "       end program Example.\n",
                ),
                generated: false,
                body_before: "display 'first'.",
                body_after: "display 'second'.",
            },
            LanguageCase {
                name: "cobol-free",
                path: "src/Example.cbl",
                frontend: "tree-sitter-cobol-free-0.0.1",
                source: concat!(
                    "identification division.\n",
                    "program-id. Example.\n",
                    "data division.\n",
                    "working-storage section.\n",
                    "01 Stored pic 9.\n",
                    "procedure division.\n",
                    "Main.\n",
                    "display 'first'.\n",
                    "goback.\n",
                    "end program Example.\n",
                ),
                generated: false,
                body_before: "display 'first'.",
                body_after: "display 'second'.",
            },
        ])
        .collect();
    let expected_languages: BTreeSet<_> = rootlight_adapter_treesitter::GrammarRegistry::audited()
        .unwrap()
        .descriptors()
        .iter()
        .filter(|descriptor| descriptor.language().as_str() != "markdown")
        .map(|descriptor| descriptor.language().as_str().to_owned())
        .collect();
    assert_eq!(
        cases
            .iter()
            .map(|case| case.name.to_owned())
            .collect::<BTreeSet<_>>(),
        expected_languages
    );
    for case in cases {
        eprintln!("checking fenced definitions for {}", case.name);
        let fixture = Fixture::new(case, case.source.as_bytes());
        let standalone = analyze(
            &analyzer(&provider, case),
            &request(&fixture.snapshot, &fixture.source, case, &budget),
            &ExtensionSupport::default(),
        );
        let source = format!(
            "# Example\n\n~~~~~~~~{}\n{}\n~~~~~~~~\n",
            case.name, case.source
        );
        let embedded = output(&source);
        let definitions = |result: &AnalysisOutput, source: &str| {
            let mut values: Vec<_> = result
                .document()
                .occurrences
                .iter()
                .filter_map(|occurrence| {
                    if occurrence.role != OccurrenceRole::Definition {
                        return None;
                    }
                    let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
                        return None;
                    };
                    let entity = result
                        .document()
                        .entities
                        .iter()
                        .find(|entity| entity.id == symbol)
                        .unwrap();
                    if entity.language != case.name {
                        return None;
                    }
                    let span = occurrence.source.span();
                    let text = &source[usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()];
                    assert_eq!(
                        occurrence.syntactic_text_hash,
                        content_hash(text.as_bytes())
                    );
                    Some((entity.kind, entity.canonical_name.clone(), text.to_owned()))
                })
                .collect();
            values.sort();
            values
        };
        let expected = definitions(&standalone, case.source);
        let embedded_definitions = definitions(&embedded, &source);
        assert_eq!(embedded_definitions, expected, "{}", case.name);
        if case.name == "dtd" {
            assert!(expected.is_empty());
            assert!(embedded_definitions.is_empty());
            let body_start = source.find(case.source).unwrap();
            for (result, text, start) in [
                (&standalone, case.source, 0),
                (&embedded, source.as_str(), body_start),
            ] {
                let document = result.document();
                assert_eq!(document.files.len(), 1);
                let file = &document.files[0];
                assert_eq!(file.content_hash, content_hash(text.as_bytes()));
                assert_eq!(file.byte_length, u64::try_from(text.len()).unwrap());
                let physical_source = file.evidence.source.as_ref().unwrap();
                assert_eq!(physical_source.repository(), document.repository);
                assert_eq!(physical_source.generation(), document.generation);
                assert_eq!(physical_source.span().file(), file.id);
                assert_eq!(physical_source.content_hash(), file.content_hash);
                assert_eq!(physical_source.span().start_byte(), 0);
                assert_eq!(physical_source.span().end_byte(), file.byte_length);
                let gaps: Vec<_> = document
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.domain == FactDomain::Entities
                            && gap.detail == "dtd-structural-declaration-kind-unavailable"
                    })
                    .collect();
                assert_eq!(gaps.len(), 1);
                let gap = gaps[0];
                assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
                assert_eq!(gap.source.repository(), document.repository);
                assert_eq!(gap.source.generation(), document.generation);
                assert_eq!(gap.source.span().file(), file.id);
                assert_eq!(gap.source.content_hash(), file.content_hash);
                assert_eq!(
                    gap.source.span().start_byte(),
                    u64::try_from(start).unwrap()
                );
                assert_eq!(
                    gap.source.span().end_byte(),
                    u64::try_from(start + case.source.len()).unwrap()
                );
                assert_eq!(
                    text.get(start..start + case.source.len()),
                    Some(case.source)
                );
                assert!(document.occurrences.iter().any(|occurrence| {
                    occurrence.role == OccurrenceRole::StringEvidence
                        && occurrence.source == gap.source
                        && occurrence.syntactic_text_hash == content_hash(case.source.as_bytes())
                }));
            }
        } else {
            assert!(!expected.is_empty(), "{}", case.name);
        }
    }
}

#[test]
fn markdown_coq_fences_keep_written_definitions_and_exact_scoped_gaps() {
    for ending in ["\n", "\r\n"] {
        let mut source = format!("# Coq examples λ😀{ending}{ending}");
        source.push_str(&format!("Definition host_decoy := 0.{ending}{ending}"));
        let mut bodies = Vec::new();
        for value in ["0", "1"] {
            source.push_str(&format!("~~~coq{ending}"));
            let start = source.len();
            source.push_str(&format!(
                "(* Definition comment_decoy := 0. *){ending}Definition résumé := {value}.{ending}"
            ));
            bodies.push(start..source.len());
            source.push_str(&format!("~~~{ending}{ending}"));
        }
        let result = output(&source);
        let document = result.document();
        let written: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.language == "coq" && entity.canonical_name == "résumé")
            .collect();
        assert_eq!(written.len(), 2);
        assert_ne!(written[0].id, written[1].id);
        assert!(
            written
                .iter()
                .all(|entity| entity.kind == EntityKind::Constant)
        );
        assert!(!document.entities.iter().any(|entity| matches!(
            entity.canonical_name.as_str(),
            "host_decoy" | "comment_decoy"
        )));
        for body in &bodies {
            let exact = |span: SourceSpan| {
                span.start_byte() == u64::try_from(body.start).unwrap()
                    && span.end_byte() == u64::try_from(body.end).unwrap()
            };
            let owners: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.language == "coq"
                        && entity.kind == EntityKind::Module
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| exact(source.span()))
                })
                .collect();
            assert_eq!(owners.len(), 1);
            let definitions: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && written.iter().any(|entity| {
                            site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                        })
                        && site.source.span().start_byte() >= u64::try_from(body.start).unwrap()
                        && site.source.span().end_byte() <= u64::try_from(body.end).unwrap()
                })
                .collect();
            assert_eq!(definitions.len(), 1);
            let definition = definitions[0];
            let start = usize::try_from(definition.source.span().start_byte()).unwrap();
            let end = usize::try_from(definition.source.span().end_byte()).unwrap();
            assert_eq!(source.get(start..end), Some("résumé"));
            assert_eq!(
                definition.source.content_hash(),
                content_hash(source.as_bytes())
            );
            assert_eq!(
                definition.syntactic_text_hash,
                content_hash("résumé".as_bytes())
            );
            assert_eq!(definition.source.repository(), document.repository);
            assert_eq!(definition.source.generation(), document.generation);
            let OccurrenceTarget::Resolved { symbol } = definition.target else {
                panic!("written Coq definition must resolve to its own source entity");
            };
            assert!(document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(owners[0].id)
                    && edge.object == RelationEndpoint::Entity(symbol)
            }));
            for (domain, detail) in [
                (
                    FactDomain::Entities,
                    "coq-local-term-binder-projection-and-generated-declaration-coverage-incomplete",
                ),
                (
                    FactDomain::Occurrences,
                    "coq-term-pattern-tactic-and-notation-reference-surface-incomplete",
                ),
                (
                    FactDomain::Relations,
                    "coq-elaboration-typechecking-proofchecking-and-term-binding-unavailable",
                ),
            ] {
                let gaps: Vec<_> = document
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.domain == domain && gap.detail == detail && exact(gap.source.span())
                    })
                    .collect();
                assert_eq!(gaps.len(), 1, "{detail}");
                assert_eq!(
                    gaps[0].source.content_hash(),
                    content_hash(source.as_bytes())
                );
                assert_eq!(gaps[0].reason, SkippedRegionReason::UnsupportedConstruct);
            }
        }
    }
}

#[test]
fn markdown_fsharp_profiles_keep_separate_written_identities_and_coverage() {
    let fixture = concat!(
        "~~~fsharp\nmodule Shared\nlet pick value = value + 1\n~~~\n\n",
        "~~~fsharp-signature\nmodule Shared\nval pick : int -> int\n~~~\n\n",
        "~~~fsharp\nmodule Shared\nlet pick value = value + 2\n~~~\n",
    );
    for ending in ["\n", "\r\n"] {
        let source = fixture.replace('\n', ending);
        let result = output(&source);
        let document = result.document();
        let written: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "pick")
            .collect();
        assert_eq!(written.len(), 3);
        assert_eq!(
            written
                .iter()
                .map(|entity| entity.id)
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
        for (language, kind, count) in [
            ("fsharp", EntityKind::Function, 2),
            ("fsharp-signature", EntityKind::Variable, 1),
        ] {
            assert_eq!(
                written
                    .iter()
                    .filter(|entity| entity.language == language && entity.kind == kind)
                    .count(),
                count
            );
        }
        for entity in written {
            let definitions: Vec<_> = document
                .occurrences
                .iter()
                .filter(|occurrence| {
                    occurrence.role == OccurrenceRole::Definition
                        && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .collect();
            assert_eq!(definitions.len(), 1);
            let span = definitions[0].source.span();
            assert_eq!(
                source.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some("pick")
            );
        }
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .filter(|gap| {
                    gap.domain == FactDomain::Relations
                        && gap.detail == "markdown-code-semantics-unavailable"
                })
                .count(),
            3
        );
        assert!(document.relations.iter().all(|relation| matches!(
            relation.predicate,
            RelationPredicate::Contains
                | RelationPredicate::Declares
                | RelationPredicate::DefinesAt
        )));
    }
}

#[test]
fn markdown_fsharp_fragments_keep_host_spans_without_whole_file_context() {
    use rootlight_adapter_sdk::execute_parse;

    for ending in ["\n", "\r\n"] {
        let mut text = format!("# λ Examples{ending}{ending}");
        let mut bodies = Vec::new();
        for (language, body) in [
            ("fsharp", "module First\nlet pick value = value + 1\n \t\n"),
            (
                "fsharp-signature",
                "module Contracts\nval pick: int -> int\n \t\n",
            ),
            ("fsharp", " \t\n"),
            ("fsharp-signature", ""),
        ] {
            text.push_str(&format!("~~~{language}{ending}"));
            let start = text.len();
            text.push_str(&body.replace('\n', ending));
            bodies.push(start..text.len());
            text.push_str(&format!("~~~{ending}{ending}"));
        }
        for encoding in ["utf-8", "utf-16le", "utf-16be"] {
            let mut bytes = match encoding {
                "utf-8" => Vec::new(),
                "utf-16le" => vec![0xff, 0xfe],
                "utf-16be" => vec![0xfe, 0xff],
                _ => unreachable!(),
            };
            if encoding == "utf-8" {
                bytes.extend_from_slice(text.as_bytes());
            } else {
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&if encoding == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    });
                }
            }
            let physical_offset = |offset| {
                if encoding == "utf-8" {
                    offset
                } else {
                    2 + 2 * text.get(..offset).unwrap().encode_utf16().count()
                }
            };
            let body_spans = bodies
                .iter()
                .map(|body| physical_offset(body.start)..physical_offset(body.end))
                .collect::<Vec<_>>();
            let provider = Arc::new(provider());
            let analyzer = analyzer(&provider, MARKDOWN);
            let fixture = Fixture::new(MARKDOWN, &bytes);
            let budget = limits();
            let initial = AnalysisRequest::new_with_parse_context(
                GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
                language(MARKDOWN),
                EncodingId::new(encoding).unwrap(),
                Vec::new(),
                AnalysisTier::TierD,
                build_context(),
                &budget,
            )
            .unwrap()
            .with_generated_status(false);
            let parsed = execute_parse(
                provider.as_ref(),
                &initial.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
            assert!(!parsed.facts().iter().any(|fact| matches!(
                fact.syntax_kind().as_str(),
                "fsharp.qualified_context.expression" | "fsharp.signature_context.expression"
            )));
            for fact in parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str().starts_with("fsharp."))
            {
                let start = usize::try_from(fact.span().start_byte()).unwrap();
                let end = usize::try_from(fact.span().end_byte()).unwrap();
                assert!(
                    body_spans
                        .iter()
                        .any(|body| body.start <= start && end <= body.end),
                    "{encoding}: {:?}",
                    fact
                );
            }
            let required = provider
                .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
                .unwrap();
            let artifact = analyzer.prepare_artifact(&initial, &deadline()).unwrap();
            assert_eq!(
                artifact.required_syntax_fact_count(&deadline()).unwrap(),
                required
            );
            assert_eq!(artifact.syntax_fact_count(), parsed.facts().len());
            let prepared = analyzer
                .analyze_prepared_artifact(
                    &initial,
                    &artifact,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
            let fresh = analyze(&analyzer, &initial, &ExtensionSupport::default());
            assert_eq!(prepared.document(), fresh.document());
            assert_eq!(prepared.report(), fresh.report());
            validate_ir_document(
                prepared.document(),
                budget.ir(),
                &ExtensionSupport::default(),
            )
            .unwrap();
            let values = prepared
                .document()
                .entities
                .iter()
                .filter(|entity| entity.canonical_name == "pick")
                .collect::<Vec<_>>();
            assert_eq!(values.len(), 2, "{encoding}");
            for (profile, kind) in [
                ("fsharp", EntityKind::Function),
                ("fsharp-signature", EntityKind::Variable),
            ] {
                let value = values
                    .iter()
                    .find(|entity| entity.language == profile)
                    .unwrap();
                assert_eq!(value.kind, kind);
                let definition = prepared
                    .document()
                    .occurrences
                    .iter()
                    .find(|occurrence| {
                        occurrence.role == OccurrenceRole::Definition
                            && occurrence.target
                                == (OccurrenceTarget::Resolved { symbol: value.id })
                    })
                    .unwrap();
                assert_eq!(
                    definition.source.content_hash(),
                    fixture.snapshot.content_hash()
                );
                assert_eq!(definition.source.generation(), fixture.source.generation());
                let span = definition.source.span();
                let start = usize::try_from(span.start_byte()).unwrap();
                let end = usize::try_from(span.end_byte()).unwrap();
                assert!(
                    body_spans
                        .iter()
                        .any(|body| body.start <= start && end <= body.end)
                );
                let expected = if encoding == "utf-8" {
                    b"pick".to_vec()
                } else {
                    "pick"
                        .encode_utf16()
                        .flat_map(|unit| {
                            if encoding == "utf-16le" {
                                unit.to_le_bytes()
                            } else {
                                unit.to_be_bytes()
                            }
                        })
                        .collect()
                };
                assert_eq!(bytes.get(start..end), Some(expected.as_slice()));
            }
            assert!(prepared.document().skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Relations
                    && gap.detail == "markdown-code-semantics-unavailable"
            }));
        }
    }
}

#[test]
fn markdown_objective_c_preserves_unary_dispatch_signatures() {
    let source = "~~~objective-c\n@interface Sample\n- (int)value;\n+ (int)value;\n@end\n~~~\n";
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
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
        .filter(|fact| fact.syntax_kind().as_str() == "objective_c.method.signature")
        .collect();
    assert_eq!(signatures.len(), 2, "{signatures:?}");
    for signature in &signatures {
        let parent = parsed
            .facts()
            .iter()
            .find(|fact| Some(fact.local_id()) == signature.parent())
            .unwrap();
        assert_eq!(
            parent.syntax_kind().as_str(),
            "objective_c.method.declaration",
            "{signature:?}; {parent:?}"
        );
    }
    let definitions: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "objective_c.method.definition")
        .collect();
    for definition in definitions {
        let parts: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == "objective_c.selector.definition_part"
                    && fact.parent() == definition.parent()
            })
            .collect();
        assert_eq!(
            rootlight_adapter_sdk::canonical_objective_c_selector(definition, &parts, source, 1024)
                .as_deref(),
            Some("value"),
            "{definition:?}; {parts:?}"
        );
    }
    let result = output(source);
    let methods: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.language == "objective-c" && entity.kind == EntityKind::Method)
        .collect();
    assert_eq!(
        methods.len(),
        2,
        "gaps: {:?}; {signatures:?}",
        result
            .document()
            .skipped_regions
            .iter()
            .map(|gap| &gap.detail)
            .collect::<Vec<_>>()
    );
    assert_ne!(methods[0].id, methods[1].id);
}

#[test]
fn markdown_data_examples_keep_addresses_and_alias_bindings_local() {
    let source = concat!(
        "~~~yaml\nname: &entry first\ncopy: *entry\n~~~\n\n",
        "~~~yaml\nname: &entry second\ncopy: *entry\n~~~\n\n",
        "~~~yaml\ncopy: *entry\n~~~\n\n",
        "~~~toml\n[config]\nname = 'first'\n~~~\n\n",
        "~~~toml\n[config]\nname = 'second'\n~~~\n",
    );
    let result = output(source);
    let document = result.document();
    let aliases: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "yaml.alias.reference")
        .collect();
    assert_eq!(aliases.len(), 3);
    let mut targets = BTreeSet::new();
    for alias in aliases {
        let start = usize::try_from(alias.source.span().start_byte()).unwrap();
        let fence_start = source[..start].rfind("~~~yaml").unwrap();
        if fence_start == source.rfind("~~~yaml").unwrap() {
            assert!(matches!(alias.target, OccurrenceTarget::Unresolved { .. }));
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.detail == "yaml-alias-target-unavailable"
                    && gap.source.span() == alias.source.span()
            }));
            continue;
        }
        let OccurrenceTarget::Resolved { symbol } = alias.target else {
            panic!("declared alias must bind within its code example");
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        let span = entity.evidence.source.as_ref().unwrap().span();
        assert!(span.start_byte() >= u64::try_from(fence_start).unwrap());
        assert!(span.end_byte() < alias.source.span().start_byte());
        targets.insert(symbol);
    }
    assert_eq!(targets.len(), 2);
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.detail == "yaml-duplicate-mapping-key" || gap.detail.contains("toml-duplicate")
    }));
    let properties: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.language == "toml" && entity.kind == EntityKind::Property)
        .collect();
    assert_eq!(properties.len(), 2);
    assert_ne!(properties[0].id, properties[1].id);
    assert_eq!(properties[0].canonical_name, properties[1].canonical_name);
    let edited = output(&source.replace("first", "a longer replacement value"));
    let data_identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .filter(|entity| matches!(entity.language.as_str(), "yaml" | "toml"))
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(data_identities(&result), data_identities(&edited));
}

#[test]
fn markdown_yaml_clean_fence_keeps_alias_key_identity_beside_recovery() {
    let healthy = "~~~yaml\nbase: &key label\n*key : value\n~~~\n";
    let broken = "~~~yaml\nbroken: [\n~~~\n";
    for source in [
        format!("{healthy}\n{broken}"),
        format!("{broken}\n{healthy}"),
    ] {
        let result = output(&source);
        assert_eq!(result.report().coverage().status(), CoverageStatus::Unknown);
        let document = result.document();
        let property = document
            .entities
            .iter()
            .find(|entity| {
                entity.language == "yaml"
                    && entity.kind == EntityKind::Property
                    && entity.canonical_name == "str:\"label\""
            })
            .expect("a certified clean fence retains its alias-key identity");
        let definition = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == (OccurrenceTarget::Resolved {
                            symbol: property.id,
                        })
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("*key")
        );
        assert!(document.occurrences.iter().any(|occurrence| {
            occurrence.syntax_kind == "yaml.alias.reference"
                && matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
        }));
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.detail == "markdown-code-parse-unavailable"
                && gap.reason == SkippedRegionReason::ParseError
        }));
    }
}

#[test]
fn markdown_yaml_clean_fence_does_not_resolve_aliases_after_native_extraction_clipping() {
    let source = format!(
        "~~~yaml\nbase: &entry value\ncopy: *entry\n*entry : alias-key-value\npadding:\n{}~~~\n\n~~~yaml\nbroken: [\n~~~\n",
        "  - \"optional text\"\n".repeat(100)
    );
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let full_budget = limits();
    let full_request = request(&fixture.snapshot, &fixture.source, MARKDOWN, &full_budget);
    let demand = provider
        .required_syntax_fact_count(&full_request.to_parse_request(), &deadline())
        .unwrap();
    let full_parse = rootlight_adapter_sdk::execute_parse(
        provider.as_ref(),
        &full_request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let scalar_count = |parsed: &rootlight_adapter_sdk::ParseOutput| {
        parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "yaml.string.string")
            .count()
    };
    assert_eq!(scalar_count(&full_parse), 100);
    let full = analyze(&analyzer, &full_request, &ExtensionSupport::default());
    assert!(full.document().occurrences.iter().any(|occurrence| {
        occurrence.syntax_kind == "yaml.alias.reference"
            && matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
    }));
    assert!(full.document().entities.iter().any(|entity| {
        entity.language == "yaml"
            && entity.kind == EntityKind::Property
            && entity.canonical_name == "str:\"value\""
    }));
    // References outrank strings, so both probe aliases survive while the bounded
    // partition drops nonredundant optional scalar evidence from the same fence.
    let budget = limits_with_syntax_records(demand + 32);
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        provider.as_ref(),
        &initial.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert!(parsed.facts().iter().any(|fact| {
        fact.syntax_kind().as_str() == "markdown.code_parsed.signature"
            && fact.span().start_byte() == 0
    }));
    assert!(
        parsed
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code().as_str() == "syntax-extraction-limit")
    );
    assert!(scalar_count(&parsed) < scalar_count(&full_parse));
    let (clipped, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        clipped.report().coverage().status(),
        CoverageStatus::Unknown
    );
    let aliases: Vec<_> = clipped
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "yaml.alias.reference")
        .collect();
    assert_eq!(aliases.len(), 2, "both probe aliases must survive clipping");
    for alias in aliases {
        assert!(matches!(alias.target, OccurrenceTarget::Unresolved { .. }));
        assert!(clipped.document().skipped_regions.iter().any(|gap| {
            gap.detail == "yaml-alias-target-unavailable" && gap.source == alias.source
        }));
        assert!(!clipped.document().relations.iter().any(|relation| {
            relation.subject == RelationEndpoint::Occurrence(alias.id)
                && relation.predicate == RelationPredicate::RefersTo
        }));
    }
    assert!(!clipped.document().entities.iter().any(|entity| {
        entity.language == "yaml"
            && entity.kind == EntityKind::Property
            && entity.canonical_name == "str:\"value\""
    }));
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        demand
    );
    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, MARKDOWN, &budget);
    let replay = analyzer
        .analyze_from_artifact(
            &next,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &next, &ExtensionSupport::default());
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    assert!(replay.document().occurrences.iter().all(|occurrence| {
        occurrence.source.generation() == changed.source.generation()
            && occurrence.source.content_hash() == changed.source.content_hash()
    }));
}

#[test]
fn markdown_malformed_yaml_fences_keep_source_owners_and_healthy_neighbors() {
    assert_malformed_data_fence_ownership(
        "yaml",
        "entry: [1,\n",
        "name: &entry usable\ncopy: *entry\n",
        "copy: *entry\n",
        false,
    );
}

#[test]
fn markdown_malformed_yaml_after_healthy_fence_keeps_its_own_source_owner() {
    assert_malformed_data_fence_ownership(
        "yaml",
        "entry: [1,\n",
        "name: &entry usable\ncopy: *entry\n",
        "copy: *entry\n",
        true,
    );
}

#[test]
fn markdown_malformed_toml_fences_keep_source_owners_and_healthy_neighbors() {
    assert_malformed_data_fence_ownership(
        "toml",
        "entry = [1,\n",
        "[config]\nname = 'usable'\n",
        "[config]\nname = 'neighbor'\n",
        false,
    );
}

fn assert_malformed_data_fence_ownership(
    language: &str,
    broken_body: &str,
    healthy_body: &str,
    neighbor_body: &str,
    healthy_first: bool,
) {
    let (bodies, broken_index, healthy_index) = if healthy_first {
        ([healthy_body, broken_body, neighbor_body], 1, 0)
    } else {
        ([broken_body, healthy_body, neighbor_body], 0, 1)
    };
    let fences = bodies.map(|body| format!("~~~{language}\n{body}~~~\n"));
    let mut offset = 0_u64;
    let ranges = fences.each_ref().map(|fence| {
        let start = offset;
        let end = start + u64::try_from(fence.len()).unwrap();
        offset = end + 1;
        (start, end)
    });
    let (broken_start, broken_end) = ranges[broken_index];
    let (healthy_start, healthy_end) = ranges[healthy_index];
    let source = fences.join("\n");
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let demand = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .expect("recoverable data fences preserve identity preflight");
    let parsed = rootlight_adapter_sdk::execute_parse(
        provider.as_ref(),
        &initial.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("recoverable data fences commit source-bound syntax");
    let (result, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("a malformed native data root must not orphan its fence");
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(parsed.report().coverage().status(), CoverageStatus::Unknown);
    assert_eq!(result.report().coverage().status(), CoverageStatus::Unknown);
    assert_eq!(artifact.syntax_fact_count(), parsed.facts().len());
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        demand
    );
    assert!(parsed.facts().iter().any(|fact| {
        fact.syntax_kind().as_str() == "markdown.code_error.signature"
            && fact.span().start_byte() == broken_start
            && fact.span().end_byte() == broken_end
    }));
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.detail == "markdown-code-parse-unavailable"
            && gap.reason == SkippedRegionReason::ParseError
            && gap.source.span().start_byte() == broken_start
            && gap.source.span().end_byte() == broken_end
    }));

    let document = result.document();
    let mut owners = BTreeSet::new();
    let header_length = u64::try_from(format!("~~~{language}\n").len()).unwrap();
    for (index, (start, end)) in ranges.iter().copied().enumerate() {
        let inside = |span: SourceSpan| start <= span.start_byte() && span.end_byte() <= end;
        let modules: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == language
                    && entity.kind == EntityKind::Module
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|site| inside(site.span()))
            })
            .collect();
        assert_eq!(
            modules.len(),
            1,
            "each data fence has one source-owning file module"
        );
        let owner = modules[0];
        let owner_source = owner.evidence.source.as_ref().unwrap();
        assert_eq!(owner_source.span().start_byte(), start + header_length);
        assert_eq!(
            owner_source.span().end_byte(),
            start + header_length + u64::try_from(bodies[index].len()).unwrap()
        );
        assert_eq!(owner_source.content_hash(), fixture.source.content_hash());
        assert_eq!(owner_source.generation(), fixture.source.generation());
        assert!(!document.occurrences.iter().any(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == (OccurrenceTarget::Resolved { symbol: owner.id })
        }));
        owners.insert(owner.id);
        for entity in document.entities.iter().filter(|entity| {
            entity.language == language
                && entity.kind != EntityKind::Module
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|site| inside(site.span()))
        }) {
            let site = entity.evidence.source.as_ref().unwrap();
            assert_eq!(site.content_hash(), fixture.source.content_hash());
            assert_eq!(site.generation(), fixture.source.generation());
            let mut current = entity;
            for _ in 0..document.entities.len() {
                let Some(rootlight_ir::ContainerRef::Entity(parent)) = current.container else {
                    panic!("embedded data must retain its source-owning module");
                };
                current = document
                    .entities
                    .iter()
                    .find(|item| item.id == parent)
                    .unwrap();
                assert!(inside(current.evidence.source.as_ref().unwrap().span()));
                if current.kind == EntityKind::Module {
                    break;
                }
            }
            assert_eq!(current.id, owner.id, "data ownership must not cross fences");
        }
    }
    assert_eq!(owners.len(), fences.len());
    for (start, end) in [ranges[healthy_index], ranges[2]] {
        assert!(document.entities.iter().any(|entity| {
            entity.language == language
                && entity.kind == EntityKind::Property
                && entity.evidence.source.as_ref().is_some_and(|site| {
                    start <= site.span().start_byte() && site.span().end_byte() <= end
                })
        }));
    }
    if language == "yaml" {
        let aliases: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "yaml.alias.reference")
            .collect();
        assert_eq!(aliases.len(), 2);
        let neighbor_start = ranges[2].0;
        for alias in aliases {
            if alias.source.span().start_byte() >= neighbor_start {
                assert!(matches!(alias.target, OccurrenceTarget::Unresolved { .. }));
            } else {
                let OccurrenceTarget::Resolved { symbol } = alias.target else {
                    panic!("a healthy fence resolves its own anchor");
                };
                let target = document
                    .entities
                    .iter()
                    .find(|entity| entity.id == symbol)
                    .unwrap();
                let span = target.evidence.source.as_ref().unwrap().span();
                assert!(healthy_start <= span.start_byte() && span.end_byte() <= healthy_end);
            }
        }
    }

    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, MARKDOWN, &budget);
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
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    let edited = fixture.rewrite(source.replace("usable", "a longer usable value").as_bytes());
    let edited_result = analyze(
        &analyzer,
        &request(&edited.snapshot, &edited.source, MARKDOWN, &budget),
        &ExtensionSupport::default(),
    );
    let data_ids = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.language == language)
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(data_ids(&result), data_ids(&edited_result));
}

#[test]
fn markdown_shader_file_roots_keep_independent_fence_source_owners() {
    let cases = [
        LanguageCase {
            name: "glsl",
            path: "src/example.glsl",
            frontend: "tree-sitter-glsl-0.2.0",
            source: "float Scale() { return 1.0; }\n",
            ..MARKDOWN
        },
        LanguageCase {
            name: "hlsl",
            path: "src/example.hlsl",
            frontend: "tree-sitter-hlsl-0.2.0",
            source: "float Scale() { return 1.0; }\n",
            ..MARKDOWN
        },
        LanguageCase {
            name: "wgsl",
            path: "src/example.wgsl",
            frontend: "tree-sitter-wgsl-0.0.9",
            source: "fn Scale() -> f32 { return 1.0; }\n",
            ..MARKDOWN
        },
    ];
    let provider = Arc::new(provider());
    let budget = limits();
    for ending in ["\n", "\r\n"] {
        let mut source = format!("# Shader examples λ😀{ending}{ending}Prelude.{ending}{ending}");
        let mut bodies = Vec::new();
        for _ in 0..2 {
            for case in cases {
                source.push_str(&format!("~~~{}{ending}", case.name));
                let start = source.len();
                source.push_str(&case.source.replace('\n', ending));
                bodies.push((case, start..source.len()));
                source.push_str(&format!("~~~{ending}{ending}Neighbor λ😀.{ending}{ending}"));
            }
        }
        source.push_str(&format!("Tail λ😀.{ending}"));
        let fixture = Fixture::new(MARKDOWN, source.as_bytes());
        let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
        let parsed = rootlight_adapter_sdk::execute_parse(
            provider.as_ref(),
            &initial.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("shader file roots belong to their included fence body");
        let result = analyze(
            &analyzer(&provider, MARKDOWN),
            &initial,
            &ExtensionSupport::default(),
        );
        validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        let document = result.document();
        let mut identities = BTreeSet::new();
        for (case, body) in &bodies {
            let start = u64::try_from(body.start).unwrap();
            let end = u64::try_from(body.end).unwrap();
            let local = |span: SourceSpan| start <= span.start_byte() && span.end_byte() <= end;
            let prefix = format!("{}.", case.name);
            for fact in parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str().starts_with(&prefix))
            {
                assert!(bodies.iter().any(|(owner, range)| {
                    owner.name == case.name
                        && fact.span().start_byte() >= u64::try_from(range.start).unwrap()
                        && fact.span().end_byte() <= u64::try_from(range.end).unwrap()
                }));
                assert_eq!(fact.span().file(), fixture.source.span().file());
            }
            for syntax in [
                format!("{}.file.module", case.name),
                format!("{}.file.scope", case.name),
            ] {
                let roots: Vec<_> = parsed
                    .facts()
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str() == syntax && local(fact.span()))
                    .collect();
                assert_eq!(roots.len(), 1, "{}: {syntax}", case.name);
                assert_eq!(roots[0].span().start_byte(), start);
                assert_eq!(roots[0].span().end_byte(), end);
            }
            if case.name == "wgsl" {
                let availability: Vec<_> = parsed
                    .facts()
                    .iter()
                    .filter(|fact| {
                        fact.syntax_kind().as_str() == "wgsl.source_availability.expression"
                            && local(fact.span())
                    })
                    .collect();
                assert_eq!(availability.len(), 1);
                assert_eq!(availability[0].span().start_byte(), start);
                assert_eq!(availability[0].span().end_byte(), end);
            }
            let functions: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.language == case.name
                        && entity.kind == EntityKind::Function
                        && entity.canonical_name == "Scale"
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|site| local(site.span()))
                })
                .collect();
            assert_eq!(functions.len(), 1, "{}", case.name);
            let function = functions[0];
            assert!(
                identities.insert(function.id),
                "same-name fences need distinct owners"
            );
            let owner_source = function.evidence.source.as_ref().unwrap();
            let owner_text = case.source.trim_end_matches('\n');
            assert_eq!(owner_source.span().start_byte(), start);
            assert_eq!(
                owner_source.span().end_byte(),
                start + u64::try_from(owner_text.len()).unwrap()
            );
            assert_eq!(owner_source.content_hash(), fixture.source.content_hash());
            assert_eq!(owner_source.generation(), fixture.source.generation());
            assert_eq!(owner_source.repository(), fixture.source.repository());
            assert_eq!(owner_source.span().file(), fixture.source.span().file());
            assert_eq!(
                source
                    .get(body.start..body.start + owner_text.len())
                    .unwrap(),
                owner_text
            );
            let definitions: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Definition && local(site.source.span()))
                .collect();
            assert_eq!(definitions.len(), 1);
            let definition = definitions[0];
            assert_eq!(
                definition.target,
                OccurrenceTarget::Resolved {
                    symbol: function.id
                }
            );
            let name_start = start + u64::try_from(owner_text.find("Scale").unwrap()).unwrap();
            assert_eq!(definition.source.span().start_byte(), name_start);
            assert_eq!(definition.source.span().end_byte(), name_start + 5);
            assert_eq!(definition.syntactic_text_hash, content_hash(b"Scale"));
            assert_eq!(
                definition.source.content_hash(),
                fixture.source.content_hash()
            );
            assert_eq!(definition.source.generation(), fixture.source.generation());
            assert_eq!(definition.source.repository(), fixture.source.repository());
            assert_eq!(
                definition.source.span().file(),
                fixture.source.span().file()
            );
            assert_eq!(
                definition.id,
                rootlight_ir::derive_occurrence_record_id(definition).unwrap()
            );

            let standalone_source = case.source.replace('\n', ending);
            let standalone = Fixture::new(*case, standalone_source.as_bytes());
            let standalone_result = analyze(
                &analyzer(&provider, *case),
                &request(&standalone.snapshot, &standalone.source, *case, &budget),
                &ExtensionSupport::default(),
            );
            let standalone_function = standalone_result
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function && entity.canonical_name == "Scale"
                })
                .unwrap();
            assert_eq!(function.kind, standalone_function.kind);
            assert_eq!(function.language, standalone_function.language);
            assert_eq!(function.canonical_name, standalone_function.canonical_name);
            let signatures = |result: &AnalysisOutput, symbol| {
                result
                    .document()
                    .extensions
                    .iter()
                    .filter_map(|envelope| {
                        if envelope.namespace != rootlight_ir::LEXICAL_EXTENSION_NAMESPACE {
                            return None;
                        }
                        let evidence =
                            rootlight_ir::decode_lexical_evidence_envelope(envelope).unwrap();
                        (evidence.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                            && evidence.subject() == rootlight_ir::FactRef::Entity(symbol))
                        .then(|| evidence.text().to_owned())
                    })
                    .collect::<Vec<_>>()
            };
            let embedded_signatures = signatures(&result, function.id);
            assert!(!embedded_signatures.is_empty());
            assert_eq!(
                embedded_signatures,
                signatures(&standalone_result, standalone_function.id)
            );
        }
        assert_eq!(identities.len(), bodies.len());
    }
}

#[test]
fn markdown_fenced_examples_keep_distinct_owners_and_original_coordinates() {
    let source = "# Examples\r\n\r\n> ~~~rust\r\n> fn greet() {}\r\n> ~~~\r\n\r\n~~~rust\r\nfn greet() {}\r\n~~~\r\n";
    let result = output(source);
    let functions: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "greet")
        .collect();
    assert_eq!(functions.len(), 2);
    assert_ne!(functions[0].id, functions[1].id);
    let mut owners = BTreeSet::new();
    for entity in functions {
        assert_eq!(entity.language, "rust");
        let source_ref = entity.evidence.source.as_ref().unwrap();
        assert_eq!(source_ref.content_hash(), content_hash(source.as_bytes()));
        let span = source_ref.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            "fn greet() {}"
        );
        let relation = result
            .document()
            .relations
            .iter()
            .find(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.object == rootlight_ir::RelationEndpoint::Entity(entity.id)
            })
            .unwrap();
        owners.insert(relation.subject);
    }
    assert_eq!(owners.len(), 2);
}

#[test]
fn markdown_named_links_and_images_bind_the_first_document_definition() {
    let source = "[text][Guide] [GUIDE][] [guide] ![alt][Guide] ![guide][] ![GUIDE]\n\n> [Guide]: first.md\n\n[guide]: second.md\n";
    let result = output(source);
    let target = result
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::LinkDefinition && entity.canonical_name == "[Guide]"
        })
        .unwrap();
    let references: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect();
    assert_eq!(references.len(), 6, "{references:#?}");
    for reference in references {
        assert_eq!(
            reference.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        let span = reference.source.span();
        assert_eq!(
            reference.syntactic_text_hash,
            content_hash(
                &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()]
            )
        );
        assert!(result.document().relations.iter().any(|relation| {
            relation.predicate == rootlight_ir::RelationPredicate::RefersTo
                && relation.subject == rootlight_ir::RelationEndpoint::Occurrence(reference.id)
                && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id)
        }));
    }
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::LinkDefinition)
            .count(),
        2
    );
}

#[test]
fn markdown_reference_comparison_preserves_unicode_and_container_source() {
    let source = "> [text][Straße\r\n> label] [É] [e\u{301}] [A\u{a0}B] [a b]\r\n\r\n[STRASSE label]: one.md\r\n[é]: two.md\r\n[a b]: three.md\r\n";
    let result = output(source);
    for (written, expected) in [
        ("[Straße\r\n> label]", Some("[STRASSE label]")),
        ("[É]", Some("[é]")),
        ("[e\u{301}]", None),
        ("[A\u{a0}B]", None),
        ("[a b]", Some("[a b]")),
    ] {
        let occurrence = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Reference
                    && occurrence.syntactic_text_hash == content_hash(written.as_bytes())
            })
            .unwrap_or_else(|| panic!("missing {written:?}"));
        let target = expected.map(|name| {
            result
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap()
                .id
        });
        if let Some(symbol) = target {
            assert_eq!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol },
                "{written}"
            );
        } else {
            assert!(
                matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }),
                "{written}"
            );
        }
    }
}

#[test]
fn markdown_bounded_plans_do_not_guess_reference_targets() {
    let source = format!(
        "> paragraph\n{}\n{}\n[ref]: target.md\n",
        "> continued\n".repeat(33),
        "[ref]\n\n".repeat(34)
    );
    let result = output(&source);
    assert_eq!(result.report().coverage().status(), CoverageStatus::Bounded);
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.detail == "markdown-inline-budget-unavailable"
            && gap.reason == SkippedRegionReason::ResourceLimit
    }));
    let references: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect();
    assert_eq!(references.len(), 34);
    for occurrence in references {
        assert!(matches!(
            occurrence.target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.detail == "markdown-reference-target-unavailable"
                && gap.source.span() == occurrence.source.span()
        }));
    }
}

#[test]
fn markdown_named_references_after_many_paragraphs_keep_exact_targets() {
    let source = format!("{}\n[ref]: target.md\n", "[ref]\n\n".repeat(257));
    let result = output(&source);
    let definition = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::LinkDefinition)
        .unwrap();
    let references: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "markdown.shortcut_link.reference")
        .collect();
    assert_eq!(references.len(), 257);
    for occurrence in references {
        assert_eq!(
            occurrence.target,
            OccurrenceTarget::Resolved {
                symbol: definition.id
            }
        );
        assert_eq!(occurrence.syntactic_text_hash, content_hash(b"[ref]"));
    }
    assert!(!result.document().skipped_regions.iter().any(|gap| {
        matches!(
            gap.detail.as_str(),
            "markdown-inline-budget-unavailable" | "markdown-reference-target-unavailable"
        )
    }));
}

#[test]
fn markdown_resolved_reference_artifacts_rebind_without_changing_targets() {
    let source = "> [text][first\n> label]\n\n> [FIRST\n> LABEL]: target.md\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, MARKDOWN, &budget);
    let replay = analyzer
        .analyze_from_artifact(
            &next,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &next, &ExtensionSupport::default());
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    let original = first
        .document()
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .unwrap();
    assert!(matches!(original.target, OccurrenceTarget::Resolved { .. }));
    let current = replay
        .document()
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .unwrap();
    assert_eq!(current.target, original.target);
    assert_eq!(current.source.generation(), changed.source.generation());
    assert_eq!(
        current.syntactic_text_hash,
        content_hash(b"[first\n> label]")
    );
}

#[test]
fn markdown_inline_evidence_preserves_written_spans_and_hashes() {
    let source = "# Links\r\n\r\n> é [full][Guide] [Guide][] [Guide] [direct](guide.md)\r\n> <https://example.test> and `code [opaque](hidden.md)`\r\n\r\n[Guide]: target.md\r\n";
    let result = output(source);
    let actual: BTreeSet<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role != OccurrenceRole::Definition)
        .map(|occurrence| {
            let span = occurrence.source.span();
            let text = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(text.as_bytes())
            );
            assert_eq!(
                occurrence.source.content_hash(),
                content_hash(source.as_bytes())
            );
            (occurrence.syntax_kind.as_str(), text)
        })
        .collect();
    for expected in [
        ("markdown.reference_label.reference", "[Guide]"),
        ("markdown.collapsed_link.reference", "[Guide][]"),
        ("markdown.shortcut_link.reference", "[Guide]"),
        ("markdown.link_destination.reference", "guide.md"),
        (
            "markdown.link_destination.reference",
            "<https://example.test>",
        ),
        ("markdown.code_span.string", "`code [opaque](hidden.md)`"),
    ] {
        assert!(
            actual.contains(&expected),
            "missing {expected:?}: {actual:#?}"
        );
    }
    assert!(
        !actual
            .iter()
            .any(|(kind, text)| kind.ends_with(".reference") && text.contains("hidden.md"))
    );
}

#[test]
fn markdown_inline_delimiters_do_not_cross_paragraphs() {
    let source = "[opening\n\nclosing](hidden.md)\n\n`[literal](also-hidden.md)`\n\n```text\n[opaque](fenced.md)\n```\n";
    let result = output(source);
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(|occurrence| { occurrence.syntax_kind != "markdown.link_destination.reference" })
    );
}

#[test]
fn markdown_inline_embedded_syntax_remains_an_exact_scoped_gap() {
    let source = "Written <i title='x'>word</i> and $x+y$.\n";
    let result = output(source);
    let gaps: BTreeSet<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "markdown-inline-embedded-unavailable")
        .map(|gap| {
            assert_eq!(gap.domain, FactDomain::Entities);
            let span = gap.source.span();
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()]
        })
        .collect();
    assert_eq!(gaps, BTreeSet::from(["<i title='x'>", "</i>", "$x+y$"]));
}

#[test]
fn markdown_native_paragraphs_do_not_consume_foreign_language_ranges() {
    let source = "[direct](guide.md)\n\n".repeat(257);
    let result = output(&source);
    assert_eq!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.syntax_kind == "markdown.link_destination.reference"
            })
            .count(),
        257
    );
    let gaps: Vec<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "markdown-inline-budget-unavailable")
        .collect();
    assert!(gaps.is_empty());
    assert_eq!(
        result.report().coverage().status(),
        output("[direct](guide.md)\n\n")
            .report()
            .coverage()
            .status()
    );
}

#[test]
fn markdown_many_quoted_paragraphs_keep_exact_unicode_source_ranges() {
    let source = "> é [direct](guide.md)\r\n> continued 🚀\r\n\r\n".repeat(257);
    let result = output(&source);
    let references: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "markdown.link_destination.reference")
        .collect();
    assert_eq!(references.len(), 257);
    for reference in references {
        let span = reference.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            "guide.md"
        );
        assert_eq!(
            reference.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(reference.syntactic_text_hash, content_hash(b"guide.md"));
    }
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "markdown-inline-budget-unavailable")
    );
}

#[test]
fn markdown_single_block_fragment_limit_keeps_other_paragraphs_available() {
    let source = format!(
        "> [blocked](blocked.md)\n{}\n[healthy](healthy.md)\n",
        "> continued\n".repeat(33)
    );
    let result = output(&source);
    let references: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "markdown.link_destination.reference")
        .collect();
    assert_eq!(references.len(), 1);
    assert_eq!(
        references[0].syntactic_text_hash,
        content_hash(b"healthy.md")
    );
    let gaps: Vec<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "markdown-inline-budget-unavailable")
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].reason, SkippedRegionReason::ResourceLimit);
    assert!(
        gaps[0].source.span().end_byte()
            < u64::try_from(source.find("[healthy]").unwrap()).unwrap()
    );
}

#[test]
fn markdown_blocks_preserve_authored_names_and_kinds() {
    let result = output(MARKDOWN.source);
    let document = result.document();
    let names: BTreeSet<_> = document
        .entities
        .iter()
        .map(|entity| (entity.kind, entity.canonical_name.as_str()))
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            (EntityKind::Module, MARKDOWN.path),
            (EntityKind::DocumentSection, "Storage *rules*"),
            (EntityKind::DocumentSection, "Shared cache"),
            (EntityKind::DocumentSection, "Other section"),
            (EntityKind::LinkDefinition, "[Guide]"),
        ])
    );
    assert!(
        document
            .entities
            .iter()
            .all(|entity| entity.language == "markdown")
    );
    assert_eq!(document.version, rootlight_ir::NormalizedIrVersion::V1_5);
    for entity in document
        .entities
        .iter()
        .filter(|entity| entity.kind != EntityKind::Module)
    {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        let text = &MARKDOWN.source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(text, entity.canonical_name);
        assert_eq!(
            definitions[0].syntactic_text_hash,
            content_hash(text.as_bytes())
        );
        assert_eq!(
            definitions[0].source.content_hash(),
            content_hash(MARKDOWN.source.as_bytes())
        );
    }
}

#[test]
fn markdown_section_ownership_follows_written_levels_including_setext() {
    let result = output(MARKDOWN.source);
    let document = result.document();
    for (name, owner, exact_text) in [
        (
            "Storage *rules*",
            MARKDOWN.path,
            MARKDOWN.source.split("Other section").next().unwrap(),
        ),
        (
            "Shared cache",
            "Storage *rules*",
            "## Shared cache\r\n\r\n[Guide]: <guide.md> \"Help\"\r\n\r\nBody.\r\n\r\n",
        ),
        (
            "[Guide]",
            "Shared cache",
            "[Guide]: <guide.md> \"Help\"\r\n",
        ),
        (
            "Other section",
            MARKDOWN.path,
            "Other section\r\n=============\r\nTail.\r\n",
        ),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let parent = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner)
            .unwrap();
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(parent.id)
                    && relation.object == RelationEndpoint::Entity(entity.id)),
            "{name} -> {owner}"
        );
        let span = entity.evidence.source.as_ref().unwrap().span();
        assert_eq!(
            &MARKDOWN.source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            exact_text
        );
    }
}

#[test]
fn markdown_nested_blocks_do_not_extend_sections_into_following_containers() {
    let source =
        "# Outer 雪\n\n> ## Quoted\n> text\n\n- ## Listed\n  item text\n\n## Following\nend\n";
    let result = output(source);
    for (name, expected) in [
        ("Quoted", "## Quoted\n> text\n"),
        ("Listed", "## Listed\n  item text\n\n"),
        ("Following", "## Following\nend\n"),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let span = entity.evidence.source.as_ref().unwrap().span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            expected,
            "{name}"
        );
    }
}

#[test]
fn markdown_empty_headings_own_sections_without_inventing_a_written_definition() {
    let result = output("#\n\nBody\n\n#\n\nOther\n");
    let headings: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::DocumentSection)
        .collect();
    assert_eq!(headings.len(), 2);
    assert_ne!(headings[0].id, headings[1].id);
    assert!(
        headings
            .iter()
            .all(|entity| entity.canonical_name == "<untitled>")
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Definition)
    );
}

#[test]
fn markdown_opaque_blocks_do_not_invent_document_headings_or_code_symbols() {
    let source = "# Written\n\n```markdown\n# Fence\n[hidden]: fake.md\n```\n\n    # Indented\n\n<!--\n# Comment\n-->\n\nParagraph with # text.\n";
    let result = output(source);
    let names: BTreeSet<_> = result
        .document()
        .entities
        .iter()
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([MARKDOWN.path, "Written", "<code-block>"])
    );
    assert_eq!(
        result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "markdown-embedded-analysis-unavailable")
            .count(),
        3
    );
}

#[test]
fn markdown_excessive_heading_name_remains_a_source_scoped_gap() {
    let source = format!(
        "# {}\n\nBody.\n\n## Visible\nTail.\n",
        "a".repeat(IrLimits::default().max_string_bytes + 1)
    );
    let result = output(&source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Visible")
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "<untitled>")
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Entities
                && gap.detail == "declaration-name-unavailable"
                && gap.source.span().start_byte() == 0
                && usize::try_from(gap.source.span().end_byte()).unwrap() == source.len())
    );
}

#[test]
fn markdown_bounded_examples_keep_preflight_and_artifact_identity_demand_equal() {
    let mut source = (0..40)
        .map(|index| format!("~~~rust\nfn example_{index}() {{}}\n~~~\n\n"))
        .collect::<String>();
    source.push_str(&format!(
        "> ~~~rust\n{}> ~~~\n",
        "> let value = 1;\n".repeat(33)
    ));
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let demand = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let (result, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.detail == "markdown-code-budget-unavailable"
            && gap.reason == SkippedRegionReason::ResourceLimit
    }));
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        demand,
        "unchanged bounded source must not change repository identity reservations"
    );
    assert!(result.document().diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "syntax-context-limit"
            && diagnostic.coverage_effect == CoverageStatus::Bounded
    }));
    assert!(
        !result
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == "syntax-extraction-limit" })
    );

    let changed = fixture.next_generation();
    let partition = limits_with_syntax_records(artifact.syntax_fact_count() + 128);
    assert!(artifact.is_compatible_with_limits(&partition));
    let next = request(&changed.snapshot, &changed.source, MARKDOWN, &partition);
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
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());

    // Intrinsic range exhaustion must not mask simultaneous record clipping.
    let bounded = limits_with_syntax_records(demand);
    let bounded_request = request(&fixture.snapshot, &fixture.source, MARKDOWN, &bounded);
    let (clipped, clipped_artifact) = analyzer
        .analyze_and_capture(
            &bounded_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert!(
        clipped
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == "syntax-extraction-limit" })
    );
    assert!(
        clipped
            .document()
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "markdown-code-budget-unavailable" })
    );
    assert!(!clipped_artifact.is_compatible_with_limits(&partition));
    assert!(
        analyzer
            .analyze_from_artifact(
                &next,
                &clipped_artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .is_err()
    );
}

#[test]
fn markdown_required_replay_preserves_ownership_and_scoped_unavailable_analysis() {
    let source =
        "# Guide\n\nUse [entry][ref].\n\n```rust\nfn embedded() {}\n```\n\n[ref]: entry.rs\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let demand = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        demand,
        artifact.required_syntax_fact_count(&deadline()).unwrap()
    );
    let bounded = limits_with_syntax_records(demand);
    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, MARKDOWN, &bounded);
    let (fresh, retained) = analyzer
        .analyze_and_capture(
            &next,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let replay = analyzer
        .analyze_from_artifact(
            &next,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    let gaps = |result: &AnalysisOutput| {
        result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail.starts_with("markdown-"))
            .map(|gap| (gap.detail.clone(), gap.domain, gap.source.span()))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(gaps(&first), gaps(&replay));
    for (detail, domain, text) in [
        (
            "markdown-link-resolution-unavailable",
            FactDomain::Relations,
            source,
        ),
        (
            "markdown-code-semantics-unavailable",
            FactDomain::Relations,
            "```rust\nfn embedded() {}\n```\n",
        ),
    ] {
        assert!(
            replay.document().skipped_regions.iter().any(|gap| {
                let span = gap.source.span();
                gap.detail == detail
                    && gap.domain == domain
                    && &source[usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()]
                        == text
            }),
            "{detail}"
        );
    }
    assert!(first.document().occurrences.iter().any(|occurrence| {
        occurrence.syntax_kind == "markdown.reference_label.reference"
            && occurrence.syntactic_text_hash == content_hash(b"[ref]")
    }));
    assert!(!replay.document().skipped_regions.iter().any(|gap| {
        gap.detail == "markdown-inline-analysis-unavailable"
            || gap.detail == "markdown-inline-parse-unavailable"
    }));
    assert!(
        replay
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "embedded"
                && entity.language == "rust"
                && entity.kind == EntityKind::Function)
    );
    assert!(
        replay
            .document()
            .skipped_regions
            .iter()
            .all(|gap| gap.source.generation() == changed.source.generation())
    );
}

#[test]
fn markdown_repeated_headings_remain_distinct_and_body_edits_preserve_identity() {
    let source = "# Cache\n\nText.\n\n## Entry\n\nFirst.\n\n## Entry\n\nSecond.\n\n[ref]: one.md\n[ref]: two.md\n";
    let before = output(source);
    let after = output(&source.replace("First.", "A longer body with é."));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.id, entity.kind, entity.canonical_name.clone()))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        before.document().entities.len(),
        6,
        "{:#?}",
        before.document()
    );
    assert_eq!(identities(&before).len(), 6);
    assert_eq!(identities(&before), identities(&after));
}
