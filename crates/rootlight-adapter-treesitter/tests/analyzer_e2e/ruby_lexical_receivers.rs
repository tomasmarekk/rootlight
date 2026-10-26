//! Program-local Ruby mutation receivers require native and physical witnesses.
//! The complete licensed original remains the first positive; unsupported paths
//! cannot borrow a target from spelling or a replayed producer certificate.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/ruby_lexical_receivers/update_pbem.rb");

fn case() -> LanguageCase {
    *CASES.iter().find(|case| case.name == "ruby").unwrap()
}

fn output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let native = Arc::new(provider());
    let case = case();
    let fixture = Fixture::new(case, bytes);
    let bounds = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &bounds,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&native, case),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), bounds.ir(), &ExtensionSupport::default()).unwrap();
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), content_hash(bytes));
        assert_eq!(occurrence.source.repository(), fixture.source.repository());
        assert_eq!(occurrence.source.generation(), fixture.source.generation());
        assert_eq!(
            occurrence.source.span().file(),
            fixture.source.span().file()
        );
    }
    result
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    start: u64,
    end: u64,
    name: &str,
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1);
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("written definition")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::Variable);
    assert_eq!(entity.canonical_name, name);
    symbol
}

fn receiver(document: &rootlight_ir::NormalizedIrDocument, start: u64, end: u64, target: SymbolId) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.syntax_kind == "ruby.lexical_receiver.reference"
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "one exact certified receiver at {start}..{end}"
    );
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    let variable = document
        .entities
        .iter()
        .find(|entity| entity.id == target)
        .unwrap();
    let Some(rootlight_ir::ContainerRef::Entity(owner)) = variable.container else {
        panic!("file module owner")
    };
    assert_eq!(sites[0].enclosing, Some(owner));
    assert_eq!(sites[0].provenance, variable.provenance);
    let module = document
        .entities
        .iter()
        .find(|entity| entity.id == owner)
        .unwrap();
    assert_eq!(module.kind, EntityKind::Module);
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(sites[0].id)
                && edge.object == RelationEndpoint::Entity(target)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&sites[0].source));
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && site.source.span() == sites[0].source.span())
    );
    assert!(
        document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(owner)
                && edge.object == RelationEndpoint::Entity(target))
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(target)
                && edge.object == RelationEndpoint::Entity(owner))
    );
}

fn no_certified_receiver(document: &rootlight_ir::NormalizedIrDocument) {
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == "ruby.lexical_receiver.reference"
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn complete_original_resolves_enums_indexed_mutation_to_its_program_definition() {
    assert_eq!(ORIGINAL.len(), 3343);
    assert_eq!(
        ORIGINAL.windows(2).filter(|part| *part == b"\r\n").count(),
        125
    );
    assert_eq!(&ORIGINAL[52..57], b"enums");
    assert_eq!(&ORIGINAL[354..359], b"enums");
    assert_eq!(
        content_hash(ORIGINAL).to_string(),
        "b3_dalqzkfz7tjtl4i5xdturmivintovzrkjzcnuqwcnokh3lqfdcvomv23pq"
    );
    let result = output(ORIGINAL, EncodingId::utf8());
    let document = result.document();
    let enums = definition(document, 52, 57, "enums");
    receiver(document, 354, 359, enums);
    definition(document, 1284, 1292, "map_type");
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 2257
                && site.source.span().end_byte() == 2268)
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "AsBaseEvent")
    );
}

#[test]
fn renamed_nested_capture_uses_program_binding_without_claiming_value_read() {
    let text = "records = {}\n[1].each do |key|\n  until false\n    if true\n      records[key][0] = 7\n    end\n  end\nend\n";
    let result = output(text.as_bytes(), EncodingId::utf8());
    let target = definition(result.document(), 0, 7, "records");
    let start = u64::try_from(text.rfind("records").unwrap()).unwrap();
    receiver(result.document(), start, start + 7, target);
}

#[test]
fn reflection_words_in_comments_and_strings_do_not_change_lexical_authority() {
    let text = "records = {}\n# eval binding local_variable_set\ntext = 'eval records = nil'\n[1].each { |key| records[key] = text }\n";
    let result = output(text.as_bytes(), EncodingId::utf8());
    let target = definition(result.document(), 0, 7, "records");
    let start = u64::try_from(text.rfind("records").unwrap()).unwrap();
    receiver(result.document(), start, start + 7, target);
}

#[test]
fn files_outside_new_proof_size_bound_keep_existing_written_definitions() {
    let mut bytes = b"records = {}\n#".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 70_000));
    bytes.extend_from_slice(b"\nrecords[0] = 7\n");
    let result = output(&bytes, EncodingId::utf8());
    definition(result.document(), 0, 7, "records");
    no_certified_receiver(result.document());
}

#[test]
fn shadow_rebinding_hard_owners_and_dynamic_binding_paths_remain_uncertified() {
    for text in [
        "records = {}\n[1].each { |records| records[0] = 7 }\n",
        "records = {}\n[1].each { |key; records| records[0] = 7 }\n",
        "records = {}\nrecords = {}\nrecords[0] = 7\n",
        "records = {}\n[1].each { records = {}; records[0] = 7 }\n",
        "records[0] = 7\nrecords = {}\n",
        "records = {}\ndef write; records[0] = 7; end\n",
        "records = {}\nclass Holder; records[0] = 7; end\n",
        "records = {}\nmodule Holder; records[0] = 7; end\n",
        "records = {}\nf = -> { records[0] = 7 }\n",
        "records = {}\nresult = [1].each { records[0] = 7 }\n",
        "records = {}\nrecords ||= {}\nrecords[0] = 7\n",
        "records, other = {}, {}\nrecords[0] = 7\n",
        "records = {}\n*records = [1, 2]\nrecords[0] = 7\n",
        "records = {}\nfor records in [{}]; records[0] = 7; end\n",
        "records = {}\neval('records = nil')\nrecords[0] = 7\n",
        "records = {}\nbinding.local_variable_set(:records, nil)\nrecords[0] = 7\n",
        "records = {}\n/(?<records>a)/ =~ 'a'\nrecords[0] = 7\n",
        "records = {}\nbegin; run; rescue => records; records[0] = 7; end\n",
    ] {
        no_certified_receiver(output(text.as_bytes(), EncodingId::utf8()).document());
    }
}

#[test]
fn index_operands_selectors_and_nonmutation_receivers_do_not_gain_this_certificate() {
    for text in [
        "records = {}\nother[records] = 7\n",
        "records = {}\nrecords[0]\n",
        "records = {}\nobject.records[0] = 7\n",
        "records = {}\nrecords(0)[0] = 7\n",
    ] {
        no_certified_receiver(output(text.as_bytes(), EncodingId::utf8()).document());
    }
}

#[test]
fn complete_crlf_bom_and_utf16_keep_physical_receiver_and_definition_spans() {
    let text = "# 🧭\r\nrecords = {}\r\n[1].each { |key| records[key] = 7 }\r\n";
    for bom in [false, true] {
        let mut bytes = if bom {
            b"\xef\xbb\xbf".to_vec()
        } else {
            Vec::new()
        };
        bytes.extend_from_slice(text.as_bytes());
        let base = u64::from(bom) * 3;
        let target = base + u64::try_from(text.find("records").unwrap()).unwrap();
        let site = base + u64::try_from(text.rfind("records").unwrap()).unwrap();
        let result = output(&bytes, EncodingId::utf8());
        let symbol = definition(result.document(), target, target + 7, "records");
        receiver(result.document(), site, site + 7, symbol);
    }
    for little in [false, true] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let offset =
            |position| 2 + u64::try_from(text[..position].encode_utf16().count() * 2).unwrap();
        let target = offset(text.find("records").unwrap());
        let site = offset(text.rfind("records").unwrap());
        let result = output(
            &bytes,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        let symbol = definition(result.document(), target, target + 14, "records");
        receiver(result.document(), site, site + 14, symbol);
    }
}

#[test]
fn error_missing_and_truncated_owners_never_certify_receivers() {
    for text in [
        "records = {}\n[1].each do |key|\nrecords[key] = 7\n",
        "records = {}\n[1].each { |key| records[key] = }\n",
        "records = {}\nclass ???\nrecords[0] = 7\nend\n",
    ] {
        no_certified_receiver(output(text.as_bytes(), EncodingId::utf8()).document());
    }
}

#[test]
fn explicit_full_selected_and_split_ranges_cannot_supply_complete_source_authority() {
    let bytes = b"records = {}\n[1].each { |key| records[key] = 7 }\n";
    let native = Arc::new(provider());
    let case = case();
    let fixture = Fixture::new(case, bytes);
    let bounds = limits();
    let selected = |start, end| {
        IncludedRange::new(
            SourceSpan::new(fixture.source.span().file(), start, end).unwrap(),
            language(case),
        )
    };
    for ranges in [
        vec![selected(0, u64::try_from(bytes.len()).unwrap())],
        vec![selected(28, 44)],
        vec![
            selected(0, 13),
            selected(13, u64::try_from(bytes.len()).unwrap()),
        ],
    ] {
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case),
            EncodingId::utf8(),
            ranges,
            AnalysisTier::TierD,
            build_context(),
            &bounds,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&native, case),
            &input,
            &ExtensionSupport::default(),
        );
        no_certified_receiver(result.document());
    }
}

#[test]
fn native_facts_replay_requires_unique_root_scopes_definition_and_coordinate_certificates() {
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};
    let native = provider();
    let case = case();
    let fixture = Fixture::new(case, ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts| {
        let mock = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(mock),
            ProducerIdentity::new(
                "ruby-receiver-replay",
                "1",
                content_hash(b"ruby-receiver-replay"),
            )
            .unwrap(),
            language(case),
            case.frontend,
            content_hash(b"ruby-receiver-binary"),
        )
        .unwrap();
        execute_analysis(
            &frontend,
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
    };
    let baseline = replay(parsed.facts().to_vec()).unwrap();
    let target = definition(baseline.document(), 52, 57, "enums");
    receiver(baseline.document(), 354, 359, target);
    for (label, start, end) in [
        ("ruby.file.root", 0, 3343),
        ("ruby.file.module", 0, 3343),
        ("ruby.file.scope", 0, 3343),
        ("ruby.block.scope", 118, 491),
        ("ruby.block.scope", 135, 487),
        ("ruby.variable.declaration", 52, 86),
        ("ruby.identifier.definition", 52, 57),
        ("ruby.lexical_receiver.reference", 354, 359),
        ("ruby.receiver_file_context.signature", 0, 3343),
        ("ruby.receiver_target.signature", 52, 359),
    ] {
        let witness = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.syntax_kind().as_str() == label
                    && fact.span().start_byte() == start
                    && fact.span().end_byte() == end
            })
            .unwrap();
        for mode in [0, 1, 2, 3] {
            let mut facts = parsed.facts().to_vec();
            if mode == 0 {
                facts.retain(|fact| fact.local_id() != witness.local_id());
            } else if mode == 1 {
                let id = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
                facts.push(SyntaxFact::new(
                    id,
                    witness.parent(),
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    witness.syntax_kind().clone(),
                ));
            } else {
                for fact in &mut facts {
                    if fact.local_id() == witness.local_id() {
                        let span = if mode == 2 {
                            SourceSpan::new(
                                fact.span().file(),
                                fact.span().start_byte() + 1,
                                fact.span().end_byte(),
                            )
                            .unwrap()
                        } else {
                            fact.span()
                        };
                        *fact = SyntaxFact::new(
                            fact.local_id(),
                            if mode == 3 { None } else { fact.parent() },
                            fact.kind(),
                            span,
                            fact.depth() + usize::from(mode == 3),
                            fact.syntax_kind().clone(),
                        );
                    }
                }
            }
            if let Ok(result) = replay(facts) {
                no_certified_receiver(result.document());
            }
        }
    }
    assert!(
        parsed
            .facts()
            .iter()
            .any(|fact| fact.kind() == SyntaxFactKind::Declaration)
    );
}

#[test]
fn exact_required_count_artifact_replay_generation_and_cancellation_keep_receiver_contract() {
    let native = Arc::new(provider());
    let case = case();
    let frontend = analyzer(&native, case);
    let fixture = Fixture::new(case, ORIGINAL);
    let wide = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &wide);
    let required = native
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let bounds = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let (result, artifact) = frontend
        .analyze_and_capture(
            &exact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let target = definition(result.document(), 52, 57, "enums");
    receiver(result.document(), 354, 359, target);
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let rebound = frontend
        .analyze_from_artifact(
            &exact,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(result.document(), rebound.document());
    let next = fixture.next_generation();
    let next_input = request(&next.snapshot, &next.source, case, &bounds);
    let rebound = frontend
        .analyze_from_artifact(
            &next_input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        rebound.document(),
        analyze(&frontend, &next_input, &ExtensionSupport::default()).document()
    );
    let small = limits_with_syntax_records(required - 1);
    let small_input = request(&fixture.snapshot, &fixture.source, case, &small);
    assert!(
        matches!(frontend.prepare_artifact(&small_input, &deadline()),
        Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts,
            observed, limit })) if observed == required && limit == required - 1)
    );
    let mut changed = ORIGINAL.to_vec();
    changed[354] = b'x';
    let rewritten = fixture.rewrite(&changed);
    assert!(
        frontend
            .analyze_from_artifact(
                &request(&rewritten.snapshot, &rewritten.source, case, &bounds),
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
    let stopped = Cancellation::new();
    stopped.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        frontend.analyze_from_artifact(
            &exact,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &stopped
        ),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn receiver_only_block_scopes_survive_exact_required_artifact_boundary() {
    let text = "records = {}\n[1].each do |key|\nrecords[key] = 7\nend\n";
    let native = Arc::new(provider());
    let case = case();
    let frontend = analyzer(&native, case);
    let fixture = Fixture::new(case, text.as_bytes());
    let wide = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &wide);
    let required = native
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let bounds = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let (result, artifact) = frontend
        .analyze_and_capture(
            &exact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let target = definition(result.document(), 0, 7, "records");
    let start = u64::try_from(text.rfind("records").unwrap()).unwrap();
    receiver(result.document(), start, start + 7, target);
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let replay = frontend
        .analyze_from_artifact(
            &exact,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(result.document(), replay.document());
}

#[test]
fn native_certificates_cannot_survive_equal_length_current_source_shadowing() {
    use rootlight_adapter_sdk::testkit::MockParseProvider;
    let before = b"records = {}\n[1].each do |unused |\nrecords[0] = 7\nend\n";
    let after = b"records = {}\n[1].each do |records|\nrecords[0] = 7\nend\n";
    assert_eq!(before.len(), after.len());
    let native = provider();
    let case = case();
    let fixture = Fixture::new(case, before);
    let bounds = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &initial.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert!(
        parsed
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "ruby.lexical_receiver.reference")
    );
    let original = output(before, EncodingId::utf8());
    let target = definition(original.document(), 0, 7, "records");
    let start = u64::try_from(
        std::str::from_utf8(before)
            .unwrap()
            .rfind("records")
            .unwrap(),
    )
    .unwrap();
    receiver(original.document(), start, start + 7, target);
    let changed = fixture.rewrite(after);
    let current = request(&changed.snapshot, &changed.source, case, &bounds);
    let mock = MockParseProvider::new(
        native.capabilities().clone(),
        parsed.facts().to_vec(),
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    let frontend = TreeSitterAnalyzer::new(
        Arc::new(mock),
        ProducerIdentity::new(
            "ruby-current-source-replay",
            "1",
            content_hash(b"ruby-current-source-replay"),
        )
        .unwrap(),
        language(case),
        case.frontend,
        content_hash(b"ruby-current-source-binary"),
    )
    .unwrap();
    if let Ok(result) = execute_analysis(
        &frontend,
        &current,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    ) {
        no_certified_receiver(result.document());
    }
}

#[test]
fn case_pattern_binders_refuse_actual_and_forced_current_source_certificates() {
    use rootlight_adapter_sdk::testkit::MockParseProvider;
    for text in [
        "records = {}\ncase input\nin records\nend\nrecords[0] = 7\n",
        "records = {}\ncase input\nin {records:}\nend\nrecords[0] = 7\n",
    ] {
        let native = provider();
        let case = case();
        let bounds = limits();
        let actual = Fixture::new(case, text.as_bytes());
        let actual_input = request(&actual.snapshot, &actual.source, case, &bounds);
        let parsed = rootlight_adapter_sdk::execute_parse(
            &native,
            &actual_input.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        let start = u64::try_from(text.rfind("records").unwrap()).unwrap();
        let at_site = |fact: &&rootlight_adapter_sdk::SyntaxFact| {
            fact.span().start_byte() == start && fact.span().end_byte() == start + 7
        };
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(at_site)
                .filter(|fact| fact.syntax_kind().as_str() == "ruby.unproven_receiver.reference")
                .count(),
            1
        );
        assert!(
            !parsed
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == "ruby.lexical_receiver.reference")
        );
        let ordinary = output(text.as_bytes(), EncodingId::utf8());
        definition(ordinary.document(), 0, 7, "records");
        no_certified_receiver(ordinary.document());

        // Same-length comment substitution supplies a genuine positive fact closure.
        let mut positive = text.as_bytes().to_vec();
        for prefix in ["case input", "in ", "end\n"] {
            positive[text.find(prefix).unwrap()] = b'#';
        }
        assert_eq!(positive.len(), text.len());
        assert_eq!(
            &positive[usize::try_from(start).unwrap()..],
            &text.as_bytes()[usize::try_from(start).unwrap()..]
        );
        let before = Fixture::new(case, &positive);
        let before_input = request(&before.snapshot, &before.source, case, &bounds);
        let certified = rootlight_adapter_sdk::execute_parse(
            &native,
            &before_input.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        for label in [
            "ruby.lexical_receiver.reference",
            "ruby.receiver_file_context.signature",
            "ruby.receiver_target.signature",
        ] {
            assert_eq!(
                certified
                    .facts()
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str() == label)
                    .count(),
                1
            );
        }
        let valid = output(&positive, EncodingId::utf8());
        let target = definition(valid.document(), 0, 7, "records");
        receiver(valid.document(), start, start + 7, target);
        let changed = before.rewrite(text.as_bytes());
        let current = request(&changed.snapshot, &changed.source, case, &bounds);
        let mock = MockParseProvider::new(
            native.capabilities().clone(),
            certified.facts().to_vec(),
            certified.diagnostics().to_vec(),
            certified.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(mock),
            ProducerIdentity::new(
                "ruby-case-pattern-replay",
                "1",
                content_hash(b"ruby-case-pattern-replay"),
            )
            .unwrap(),
            language(case),
            case.frontend,
            content_hash(b"ruby-case-pattern-binary"),
        )
        .unwrap();
        let (fresh, artifact) = frontend
            .analyze_and_capture(
                &current,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let retained = frontend
            .analyze_from_artifact(
                &current,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(fresh.document(), retained.document());
        for result in [&fresh, &retained] {
            validate_ir_document(result.document(), bounds.ir(), &ExtensionSupport::default())
                .unwrap();
            no_certified_receiver(result.document());
            assert!(!result.document().occurrences.iter().any(|site| site.role
                == OccurrenceRole::Reference
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == start + 7
                && matches!(site.target, OccurrenceTarget::Resolved { .. })));
        }
    }
}
