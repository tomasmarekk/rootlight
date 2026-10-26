//! Dynamic shell constructs retain unresolved variable targets.

use super::*;

fn run(raw: &[u8]) -> AnalysisOutput {
    let parser = Arc::new(provider());
    let fixture = Fixture::new(BASH_CASE, raw);
    let output = analyze(
        &analyzer(&parser, BASH_CASE),
        &request(&fixture.snapshot, &fixture.source, BASH_CASE, &limits()),
        &ExtensionSupport::default(),
    );
    validate_ir_document(
        output.document(),
        limits().ir(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    output
}

#[test]
fn dynamic_scope_source_eval_reassignment_and_loop_writes_keep_targets_unproved() {
    for raw in [
        "WRITTEN=outer\nfunction use_it(){ printf '%s' \"$WRITTEN\"; }\nuse_it\n",
        "WRITTEN=outer\nsource other.sh\nprintf '%s' \"$WRITTEN\"\n",
        "WRITTEN=outer\neval other\nprintf '%s' \"$WRITTEN\"\n",
        "WRITTEN=outer\nWRITTEN=later\nprintf '%s' \"$WRITTEN\"\n",
        "WRITTEN=outer\nfor WRITTEN in one two; do printf '%s' \"$WRITTEN\"; done\n",
        "WRITTEN=outer\n\"$dynamic\"\nprintf '%s' \"$WRITTEN\"\n",
        "WRITTEN=outer\nchange(){ WRITTEN=other; }\nchange\nprintf '%s' \"$WRITTEN\"\n",
    ] {
        let output = run(raw.as_bytes());
        let start = raw.rfind("$WRITTEN").unwrap() as u64 + 1;
        assert!(
            !output.document().occurrences.iter().any(|site| {
                matches!(site.role, OccurrenceRole::Reference | OccurrenceRole::Read)
                    && site.source.span().start_byte() == start
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            }),
            "{raw}"
        );
    }
}

// Companion Reference contracts preserve the original physical source and Reads.
// These tests join exact authored definitions; lexical spelling is only a test anchor.

const VERSION_ORIGINAL: &[u8] = include_bytes!("../fixtures/bash_module_references/tag_version.sh");
const MODULE_REFERENCE: &str = "bash.module_reference.reference";

fn exact_module_references(
    output: &AnalysisOutput,
    raw: &[u8],
    name: &str,
    definition: (usize, usize),
    uses: &[(usize, usize)],
) -> SymbolId {
    let document = output.document();
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == u64::try_from(definition.0).unwrap()
                && site.source.span().end_byte() == u64::try_from(definition.1).unwrap()
        })
        .collect();
    assert_eq!(definitions.len(), 1, "one exact authored definition");
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("authored definition must resolve")
    };
    let entities: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.id == symbol
                && entity.language == "bash"
                && entity.kind == EntityKind::Variable
                && entity.canonical_name == name
        })
        .collect();
    assert_eq!(entities.len(), 1);
    assert_eq!(definitions[0].source.content_hash(), content_hash(raw));
    for &(start, end) in uses {
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(end).unwrap()
            })
            .collect();
        assert_eq!(sites.len(), 1, "one companion Reference at {start}..{end}");
        let site = sites[0];
        assert_eq!(site.syntax_kind, MODULE_REFERENCE);
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
        assert_eq!(site.source.content_hash(), content_hash(raw));
        assert_eq!(
            site.id,
            rootlight_ir::derive_occurrence_record_id(site).unwrap()
        );
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(symbol)
                    && edge.evidence.source.as_ref() == Some(&site.source)
            })
            .collect();
        assert_eq!(
            edges.len(),
            1,
            "one exact occurrence-owned Reference relation"
        );
        assert_eq!(
            edges[0].id,
            rootlight_ir::derive_relation_record_id(edges[0]).unwrap()
        );
        assert!(!document.relations.iter().any(|edge|
            edge.predicate == RelationPredicate::RefersTo
                && matches!(edge.subject, RelationEndpoint::Entity(owner)
                    if document.entities.iter().any(|entity| entity.id == owner && entity.kind == EntityKind::Variable))
                && edge.evidence.source.as_ref().is_some_and(|source| source.span() == site.source.span())),
            "a command/test Reference must not invent an assignment-owner dependency");
    }
    symbol
}

fn expansion_spans(source: &str, name: &str) -> Vec<(usize, usize)> {
    let marker = format!("${name}");
    source
        .match_indices(&marker)
        .map(|(start, _)| (start + 1, start + 1 + name.len()))
        .collect()
}

#[test]
fn complete_original_version_has_four_exact_references_and_preserves_two_reads() {
    assert_eq!(VERSION_ORIGINAL.len(), 343);
    assert_eq!(
        VERSION_ORIGINAL
            .windows(2)
            .filter(|pair| *pair == b"\r\n")
            .count(),
        16
    );
    let output = run(VERSION_ORIGINAL);
    let symbol = exact_module_references(
        &output,
        VERSION_ORIGINAL,
        "VER",
        (147, 150),
        &[(168, 171), (273, 276), (330, 333), (337, 340)],
    );
    let reads: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.target == (OccurrenceTarget::Resolved { symbol })
        })
        .collect();
    assert_eq!(
        reads.len(),
        2,
        "preserve the independently observed existing resolved Reads"
    );
    let mut spans = reads
        .iter()
        .map(|read| {
            (
                read.source.span().start_byte(),
                read.source.span().end_byte(),
            )
        })
        .collect::<Vec<_>>();
    spans.sort_unstable();
    assert_eq!(spans, [(330, 333), (337, 340)]);
}

#[test]
fn renamed_string_file_regex_branch_and_numeric_descriptor_uses_prove_companions() {
    for body in [
        "if [ \"$OWNED\" = value ]; then printf '%s' \"$OWNED\"; fi",
        "if ! [[ -n \"$OWNED\" ]]; then printf '%s' \"$OWNED\" 1>&2; fi",
        "if [[ -f \"$OWNED\" ]]; then printf '%s' \"$OWNED\"; fi",
        "if [[ \"$OWNED\" =~ ^[a-z]+$ ]]; then printf '%s' \"$OWNED\"; fi",
        "if [ $# -ne 2 ]; then printf '%s' \"$OWNED\"; fi",
        "if [[ \"$OWNED\" = value ]]; then printf '%s' \"$OWNED\"; elif [[ -n \"$OWNED\" ]]; then printf '%s' \"$OWNED\"; else printf '%s' \"$OWNED\"; fi",
        "cleanup(){ unset OWNED; }\nprintf '%s' \"$OWNED\"",
        "OTHER='&'\n# literal &\nprintf '%s' \"$OWNED\"",
    ] {
        let source = format!("OWNED=value\n{body}\nprintf '%s' \"$OWNED\"\n");
        let output = run(source.as_bytes());
        exact_module_references(
            &output,
            source.as_bytes(),
            "OWNED",
            (0, 5),
            &expansion_spans(&source, "OWNED"),
        );
    }
}

#[test]
fn unproved_effects_and_ordinary_variable_arithmetic_never_gain_companions() {
    for effect in [
        "source other.sh",
        ". other.sh",
        "eval other",
        "alias printf=other",
        "unalias printf",
        "unset OWNED",
        "declare -n ALIAS=OWNED",
        "declare -n OWNED=other",
        "printf -v OWNED changed",
        "printf '-v' OWNED changed",
        "printf '%n' OWNED",
        "printf '%s%n' value OWNED",
        "FORMAT='%s'\nprintf \"$FORMAT\" value",
        "OWNED=later",
        "if [[ -n ready ]]; then OWNED=later; fi",
        "for OWNED in a b; do :; done",
        "while false; do OWNED=later; done",
        "((OWNED++))",
        "((++OWNED))",
        "((OWNED+=1))",
        "printf '%s' ${OWNED:=changed}",
        "ARITH='OWNED++'\n[[ $ARITH -eq 0 ]]",
        "ARITH='++OWNED'\n[[ 0 -lt $ARITH ]]",
        "change(){ OWNED=later; }\nchange",
        "\"$runner\"",
        "mystery_operation",
        "OWNED[0]=later",
        "OWNED=(a b)",
        "OWNED=value &",
        "printf x &",
        "(OWNED=later)",
        "printf x | cat",
        "cat <(other)",
        "{fd}>output.txt",
    ] {
        let source = format!("OWNED=value\n{effect}\nprintf '%s' \"$OWNED\"\n");
        let output = run(source.as_bytes());
        let start = u64::try_from(source.rfind("$OWNED").unwrap() + 1).unwrap();
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == start
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
        assert!(
            !output.document().relations.iter().any(|edge| edge.predicate
                == RelationPredicate::RefersTo
                && edge
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().start_byte() == start)
                && matches!(edge.subject, RelationEndpoint::Entity(_))),
            "{source}"
        );
    }
}

#[test]
fn regex_effect_does_not_preserve_a_prior_bash_rematch_binding() {
    let source = b"BASH_REMATCH=stale\n[[ text =~ ^text$ ]]\nprintf '%s' \"$BASH_REMATCH\"\n";
    let output = run(source);
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == MODULE_REFERENCE
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn documentary_complex_and_unproved_owner_paths_do_not_create_companions() {
    for body in [
        "printf '%s' '$OWNED'",
        "printf '%s' \"\\$OWNED\"",
        "# $OWNED",
        "cat <<'END'\n$OWNED\nEND",
        "printf '%s' ${!OWNED}",
        "printf '%s' ${OWNED[0]}",
        "printf '%s' ${OWNED:-fallback}",
        "f(){ printf '%s' \"$OWNED\"; }",
        "(printf '%s' \"$OWNED\")",
        "printf '%s' \"$OWNED\" | cat",
        "cat <(printf '%s' \"$OWNED\")",
        "printf x >&\"$OWNED\"",
    ] {
        let source = format!("OWNED=value\n{body}\n");
        let output = run(source.as_bytes());
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == MODULE_REFERENCE
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
    }
}

#[test]
fn selected_and_split_ranges_cannot_hide_an_earlier_module_mutation() {
    let source = b"OWNED=value\neval unknown\nprintf '%s' \"$OWNED\"\n";
    let provider = Arc::new(provider());
    let fixture = Fixture::new(BASH_CASE, source);
    let budget = limits();
    let final_start = source
        .windows(b"printf".len())
        .position(|part| part == b"printf")
        .unwrap();
    for spans in [
        vec![(final_start, source.len())],
        vec![(0, 12), (final_start, source.len())],
    ] {
        let ranges = spans
            .into_iter()
            .map(|(start, end)| {
                IncludedRange::new(
                    SourceSpan::new(
                        fixture.source.span().file(),
                        u64::try_from(start).unwrap(),
                        u64::try_from(end).unwrap(),
                    )
                    .unwrap(),
                    language(BASH_CASE),
                )
            })
            .collect();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(BASH_CASE),
            EncodingId::utf8(),
            ranges,
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let output = analyze(
            &analyzer(&provider, BASH_CASE),
            &request,
            &ExtensionSupport::default(),
        );
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == MODULE_REFERENCE
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
    }
}

#[test]
fn captured_companions_requalify_current_bytes_and_independent_preflight() {
    let source = b"OWNED=value\nif [[ -n \"$OWNED\" ]]; then printf '%s' \"$OWNED\" 1>&2; fi\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, BASH_CASE);
    let fixture = Fixture::new(BASH_CASE, source);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, BASH_CASE, &budget);
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let text = std::str::from_utf8(source).unwrap();
    exact_module_references(
        &fresh,
        source,
        "OWNED",
        (0, 5),
        &expansion_spans(text, "OWNED"),
    );
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap()
    );
    let next = fixture.next_generation();
    let current = request(&next.snapshot, &next.source, BASH_CASE, &budget);
    let replay = analyzer
        .analyze_from_artifact(
            &current,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    exact_module_references(
        &replay,
        source,
        "OWNED",
        (0, 5),
        &expansion_spans(text, "OWNED"),
    );
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyzer.analyze_from_artifact(
            &current,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled
        ),
        Err(AdapterError::Cancelled { .. })
    ));
    let changed = fixture.rewrite(b"OWNED=value\neval unknown\nprintf '%s' \"$OWNED\"\n");
    let changed_request = request(&changed.snapshot, &changed.source, BASH_CASE, &budget);
    assert!(
        analyzer
            .analyze_from_artifact(
                &changed_request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err(),
        "a retained positive artifact is not current-source authority for different bytes"
    );
}

#[test]
fn companion_references_map_bom_crlf_and_utf16_names_to_physical_bytes() {
    let logical = "# marker ÃŽÂ»Ã°Å¸Ëœâ‚¬\r\nOWNED=value\r\nif [[ -n \"$OWNED\" ]]; then printf '%s' \"$OWNED\" 1>&2; fi\r\n";
    for (raw, encoding, width) in [
        (
            [b"\xef\xbb\xbf".as_slice(), logical.as_bytes()].concat(),
            EncodingId::utf8(),
            1usize,
        ),
        (
            [
                b"\xff\xfe".as_slice(),
                logical
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>()
                    .as_slice(),
            ]
            .concat(),
            EncodingId::new("utf-16le").unwrap(),
            2,
        ),
        (
            [
                b"\xfe\xff".as_slice(),
                logical
                    .encode_utf16()
                    .flat_map(u16::to_be_bytes)
                    .collect::<Vec<_>>()
                    .as_slice(),
            ]
            .concat(),
            EncodingId::new("utf-16be").unwrap(),
            2,
        ),
    ] {
        let provider = Arc::new(provider());
        let fixture = Fixture::new(BASH_CASE, &raw);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(BASH_CASE),
            encoding,
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let output = analyze(
            &analyzer(&provider, BASH_CASE),
            &request,
            &ExtensionSupport::default(),
        );
        let mapped = |offset: usize| {
            if width == 1 {
                3 + offset
            } else {
                2 + 2 * logical[..offset].encode_utf16().count()
            }
        };
        let definition = logical.find("OWNED=").unwrap();
        let uses: Vec<_> = expansion_spans(logical, "OWNED")
            .into_iter()
            .map(|(start, end)| (mapped(start), mapped(end)))
            .collect();
        exact_module_references(
            &output,
            &raw,
            "OWNED",
            (mapped(definition), mapped(definition + 5)),
            &uses,
        );
    }
}

#[test]
fn shell_owned_names_do_not_gain_ordinary_assignment_identity() {
    for name in [
        "_",
        "RANDOM",
        "SRANDOM",
        "SECONDS",
        "LINENO",
        "SHELLOPTS",
        "BASHOPTS",
    ] {
        let source = format!("{name}=value\nprintf '%s' \"${name}\"\n");
        let output = run(source.as_bytes());
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == MODULE_REFERENCE
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
    }
}

#[test]
fn simple_braced_names_preserve_exact_written_name_spans() {
    let source = b"RENAMED=value\nprintf '%s' \"${RENAMED}\"\n";
    let output = run(source);
    let start = source
        .windows(b"${RENAMED}".len())
        .position(|part| part == b"${RENAMED}")
        .unwrap()
        + 2;
    exact_module_references(&output, source, "RENAMED", (0, 7), &[(start, start + 7)]);
}

// Exercise actual native producer facts through the existing SDK replay and
// public capture/replay path; synthetic standalone joins are a separate control.
#[test]
fn native_companion_certificates_refuse_tampering_without_losing_old_reads_or_calls() {
    use rootlight_adapter_sdk::{
        SyntaxFact, SyntaxFactKind, SyntaxKindLabel, execute_parse, testkit::MockParseProvider,
    };
    use rootlight_ids::FileId;
    let wrapper =
        b"target(){ :; }\nwrapper(){ >&2 target \"$@\"; }\nOWNED=value\nprintf '%s' \"$OWNED\"\n";
    for (source, name, definition, uses, read_count, call_count) in [
        (
            VERSION_ORIGINAL,
            "VER",
            (147, 150),
            vec![(168, 171), (273, 276), (330, 333), (337, 340)],
            2,
            0,
        ),
        (wrapper.as_slice(), "OWNED", (45, 50), vec![(71, 76)], 1, 1),
    ] {
        let native = provider();
        let fixture = Fixture::new(BASH_CASE, source);
        let budget = limits();
        let input = request(&fixture.snapshot, &fixture.source, BASH_CASE, &budget);
        let parsed = execute_parse(
            &native,
            &input.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        let replay = |facts: Vec<SyntaxFact>| {
            let parser = MockParseProvider::new(
                native.capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let frontend = TreeSitterAnalyzer::new(
                Arc::new(parser),
                producer_identity(),
                language(BASH_CASE),
                BASH_CASE.frontend,
                content_hash(BINARY_SEED),
            )
            .unwrap();
            let (fresh, artifact) = frontend.analyze_and_capture(
                &input,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )?;
            let retained = frontend.analyze_from_artifact(
                &input,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )?;
            assert_eq!(fresh.document(), retained.document());
            Ok::<AnalysisOutput, AdapterError>(retained)
        };
        let baseline = replay(parsed.facts().to_vec()).unwrap();
        exact_module_references(&baseline, source, name, definition, &uses);
        let old_sites = |output: &AnalysisOutput| {
            output
                .document()
                .occurrences
                .iter()
                .filter(|site| {
                    matches!(site.role, OccurrenceRole::Read | OccurrenceRole::CallSite)
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let preserved = old_sites(&baseline);
        assert_eq!(
            preserved
                .iter()
                .filter(|site| site.role == OccurrenceRole::Read)
                .count(),
            read_count
        );
        assert_eq!(
            preserved
                .iter()
                .filter(|site| site.role == OccurrenceRole::CallSite)
                .count(),
            call_count
        );
        let certificates = parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == MODULE_REFERENCE)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            certificates.len(),
            uses.len(),
            "actual producer must supply each positive before tampering"
        );
        for mutation in 0..6 {
            let mut facts = parsed.facts().to_vec();
            match mutation {
                0 => facts.retain(|fact| fact.syntax_kind().as_str() != MODULE_REFERENCE),
                1 => {
                    for certificate in &certificates {
                        let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
                        facts.push(SyntaxFact::new(
                            next,
                            certificate.parent(),
                            certificate.kind(),
                            certificate.span(),
                            certificate.depth(),
                            certificate.syntax_kind().clone(),
                        ));
                    }
                }
                _ => {
                    let root = facts
                        .iter()
                        .find(|fact| fact.kind() == SyntaxFactKind::Root)
                        .unwrap()
                        .clone();
                    for fact in &mut facts {
                        if fact.syntax_kind().as_str() != MODULE_REFERENCE {
                            continue;
                        }
                        let span = match mutation {
                            3 => SourceSpan::new(
                                fact.span().file(),
                                u64::try_from(definition.0).unwrap(),
                                u64::try_from(definition.1).unwrap(),
                            )
                            .unwrap(),
                            4 => SourceSpan::new(
                                FileId::from_bytes([77; 20]),
                                fact.span().start_byte(),
                                fact.span().end_byte(),
                            )
                            .unwrap(),
                            _ => fact.span(),
                        };
                        *fact = SyntaxFact::new(
                            fact.local_id(),
                            if mutation == 5 {
                                Some(root.local_id())
                            } else {
                                fact.parent()
                            },
                            fact.kind(),
                            span,
                            if mutation == 5 {
                                root.depth() + 1
                            } else {
                                fact.depth()
                            },
                            if mutation == 2 {
                                SyntaxKindLabel::new("bash.unproven_module_reference.reference")
                                    .unwrap()
                            } else {
                                fact.syntax_kind().clone()
                            },
                        );
                    }
                }
            }
            let result = replay(facts);
            if mutation == 4 {
                assert!(
                    result.is_err(),
                    "foreign source-frame certificates must fail public admission"
                );
                continue;
            }
            let output = result.unwrap();
            assert!(
                !output
                    .document()
                    .occurrences
                    .iter()
                    .any(|site| site.syntax_kind == MODULE_REFERENCE
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })),
                "no new certified Reference after native certificate mutation {mutation}"
            );
            assert_eq!(
                old_sites(&output),
                preserved,
                "old exact Read/Call records survive mutation {mutation}"
            );
            validate_ir_document(output.document(), budget.ir(), &ExtensionSupport::default())
                .unwrap();
        }
    }
}

// Actual native producer facts and a forged positive artifact must agree on the
// complete current source; legacy Read/Calls are an independent contract.
#[test]
fn implicit_arithmetic_and_readonly_assignments_refuse_native_and_replayed_companions() {
    use rootlight_adapter_sdk::{
        SyntaxFact, SyntaxKindLabel, execute_parse, testkit::MockParseProvider,
    };
    let mut cases = vec![
        (
            include_str!(
                "../fixtures/bash_module_references/companion_refusal/indexed-arithmetic.sh.txt"
            )
            .to_owned(),
            "OWNED",
        ),
        (
            include_str!(
                "../fixtures/bash_module_references/companion_refusal/substring-arithmetic.sh.txt"
            )
            .to_owned(),
            "OWNED",
        ),
        (
            "OWNED=1\nINDEX='OWNED++'\n: \"${ARR[INDEX]}\"\nprintf '%s' \"$OWNED\"\n".to_owned(),
            "OWNED",
        ),
        (
            "OWNED=1\nSHIFT='++OWNED'\nTEXT=abc\n: \"${TEXT:1:SHIFT}\"\nprintf '%s' \"$OWNED\"\n"
                .to_owned(),
            "OWNED",
        ),
        (
            "OWNED=1\nINDEX='OWNED++'\nARR=([INDEX]=first)\nprintf '%s' \"$OWNED\"\n".to_owned(),
            "OWNED",
        ),
    ];
    for name in ["UID", "EUID", "PPID", "BASH_VERSINFO", "BASH_MONOSECONDS"] {
        cases.push((format!("{name}=7\nprintf '%s' \"${name}\"\n"), name));
    }
    for (source, name) in cases {
        let native = provider();
        let fixture = Fixture::new(BASH_CASE, source.as_bytes());
        let budget = limits();
        let input = request(&fixture.snapshot, &fixture.source, BASH_CASE, &budget);
        let parsed = execute_parse(
            &native,
            &input.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete,
            "{source}"
        );
        let start = u64::try_from(source.rfind(name).unwrap()).unwrap();
        let selected = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == "bash.unproven_module_reference.reference"
                    && fact.span().start_byte() == start
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            selected.len(),
            1,
            "actual producer must explicitly refuse the final simple use: {source}"
        );
        let replay = |facts: Vec<SyntaxFact>| {
            let parser = MockParseProvider::new(
                native.capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let frontend = TreeSitterAnalyzer::new(
                Arc::new(parser),
                producer_identity(),
                language(BASH_CASE),
                BASH_CASE.frontend,
                content_hash(BINARY_SEED),
            )
            .unwrap();
            let (fresh, artifact) = frontend
                .analyze_and_capture(
                    &input,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
            let retained = frontend
                .analyze_from_artifact(
                    &input,
                    &artifact,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
            assert_eq!(fresh.document(), retained.document());
            retained
        };
        let baseline = replay(parsed.facts().to_vec());
        let old_sites = |output: &AnalysisOutput| {
            output
                .document()
                .occurrences
                .iter()
                .filter(|site| matches!(site.role, OccurrenceRole::Read | OccurrenceRole::CallSite))
                .cloned()
                .collect::<Vec<_>>()
        };
        let preserved = old_sites(&baseline);
        let mut forged = parsed.facts().to_vec();
        let certificate = forged
            .iter_mut()
            .find(|fact| fact.local_id() == selected[0].local_id())
            .unwrap();
        *certificate = SyntaxFact::new(
            certificate.local_id(),
            certificate.parent(),
            certificate.kind(),
            certificate.span(),
            certificate.depth(),
            SyntaxKindLabel::new(MODULE_REFERENCE).unwrap(),
        );
        let refused = replay(forged);
        for output in [&baseline, &refused] {
            assert!(
                !output
                    .document()
                    .occurrences
                    .iter()
                    .any(|site| site.syntax_kind == MODULE_REFERENCE
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })),
                "{source}"
            );
            validate_ir_document(output.document(), budget.ir(), &ExtensionSupport::default())
                .unwrap();
        }
        assert_eq!(
            old_sites(&refused),
            preserved,
            "new refusal cannot change legacy Read/Call records: {source}"
        );
    }
}

#[test]
fn literal_complex_text_uncalled_bodies_and_simple_braces_keep_companions() {
    for body in [
        ": '${TEXT:OFFSET}' '${ARR[INDEX]}'",
        "# ${TEXT:OFFSET} ${ARR[INDEX]}\n:",
        "unused(){ : \"${ARR[INDEX]}\" \"${TEXT:OFFSET}\"; }",
        ": \"${OWNED}\"",
    ] {
        let source = format!("OWNED=value\n{body}\nprintf '%s' \"$OWNED\"\n");
        let output = run(source.as_bytes());
        let final_start = source.rfind("$OWNED").unwrap() + 1;
        exact_module_references(
            &output,
            source.as_bytes(),
            "OWNED",
            (0, 5),
            &[(final_start, final_start + 5)],
        );
    }
    let name = "BASH_CUSTOM";
    let source = format!("{name}=written\nprintf '%s' \"${name}\"\n");
    let output = run(source.as_bytes());
    let start = source.rfind(name).unwrap();
    exact_module_references(
        &output,
        source.as_bytes(),
        name,
        (0, name.len()),
        &[(start, start + name.len())],
    );
}

// The complete build script keeps its later loop/effects in the admitted frame.
// A lexical variable Reference supplies no authority for the command it selects.
const BUILD_ORIGINAL: &[u8] =
    include_bytes!("../fixtures/bash_module_references/carbon_build/build.sh");

fn complete_build_original_outputs() -> (AnalysisOutput, AnalysisOutput) {
    assert_eq!(BUILD_ORIGINAL.len(), 498);
    assert_eq!(
        BUILD_ORIGINAL
            .windows(2)
            .filter(|pair| *pair == b"\r\n")
            .count(),
        16
    );
    assert_eq!(&BUILD_ORIGINAL[73..80], b"BIN_DIR");
    assert_eq!(&BUILD_ORIGINAL[101..108], b"BIN_DIR");
    assert_eq!(&BUILD_ORIGINAL[14..29], b"CARBON_COMPILER");
    assert_eq!(&BUILD_ORIGINAL[193..208], b"CARBON_COMPILER");
    let parser = Arc::new(provider());
    let fixture = Fixture::new(BASH_CASE, BUILD_ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, BASH_CASE, &budget);
    let frontend = analyzer(&parser, BASH_CASE);
    let (fresh, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let retained = frontend
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fresh.document(), retained.document());
    for output in [&fresh, &retained] {
        validate_ir_document(output.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    }
    (fresh, retained)
}

#[test]
fn complete_original_build_argument_has_exact_module_reference_and_replays() {
    let (fresh, retained) = complete_build_original_outputs();
    let symbol =
        exact_module_references(&fresh, BUILD_ORIGINAL, "BIN_DIR", (73, 80), &[(101, 108)]);
    assert_eq!(
        exact_module_references(
            &retained,
            BUILD_ORIGINAL,
            "BIN_DIR",
            (73, 80),
            &[(101, 108)],
        ),
        symbol
    );
}

#[test]
fn complete_original_build_command_name_does_not_gain_a_resolved_reference() {
    let (fresh, retained) = complete_build_original_outputs();
    for output in [&fresh, &retained] {
        exact_module_references(output, BUILD_ORIGINAL, "CARBON_COMPILER", (14, 29), &[]);
        // Unsupported command-name paths may omit a Reference or keep it
        // unproved. The existing Read/Call records remain an independent contract.
        for site in output.document().occurrences.iter().filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == 193
                && site.source.span().end_byte() == 208
        }) {
            assert!(!matches!(site.target, OccurrenceTarget::Resolved { .. }));
            assert!(!output.document().relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && matches!(edge.object, RelationEndpoint::Entity(_))
            }));
        }
    }
}
