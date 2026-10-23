//! A written Eiffel formal receiver has a Read facet independent of feature dispatch.
//! The complete original corpus file pins the unresolved Reference and exact target.

use super::*;

const EIFFEL: LanguageCase = LanguageCase {
    name: "eiffel",
    path: "library/kernel/src/basic/kl_boolean_routines.e",
    frontend: "tree-sitter-eiffel-1.0.0",
    source: "class SAMPLE feature use (value: ANY) do value.run end end\n",
    generated: false,
    body_before: "value.run",
    body_after: "value.other",
};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/eiffel_formal_receiver/kl_boolean_routines.e");

fn output(bytes: &[u8]) -> AnalysisOutput {
    output_encoding(bytes, EncodingId::utf8())
}

fn output_encoding(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(EIFFEL, bytes);
    let budget = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(EIFFEL),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, EIFFEL),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn assert_receiver(
    result: &AnalysisOutput,
    bytes: &[u8],
    name: &str,
    definition: (u64, u64),
    receiver: (u64, u64),
    spelling: &[u8],
) {
    let document = result.document();
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == definition.0
                && site.source.span().end_byte() == definition.1
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("formal definition resolves")
    };
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(parameter.kind, EntityKind::Parameter);
    assert_eq!(parameter.canonical_name, name);
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == receiver.0
                && site.source.span().end_byte() == receiver.1
        })
        .collect();
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == receiver.0
                && site.source.span().end_byte() == receiver.1
        })
        .collect();
    assert_eq!(references.len(), 1);
    assert_eq!(reads.len(), 1);
    assert_eq!(references[0].syntax_kind, "eiffel.identifier.reference");
    assert!(matches!(
        references[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    let read = reads[0];
    assert_eq!(read.syntax_kind, "eiffel.formal_receiver.read");
    assert_eq!(read.target, OccurrenceTarget::Resolved { symbol });
    assert_ne!(read.id, references[0].id);
    assert_eq!(read.source, references[0].source);
    assert_eq!(read.provenance, references[0].provenance);
    assert_eq!(read.source.content_hash(), content_hash(bytes));
    assert_eq!(read.syntactic_text_hash, content_hash(spelling));
    assert_eq!(read.evidence.source.as_ref(), Some(&read.source));
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(read.id)
                && edge.object == RelationEndpoint::Entity(symbol)
                && edge.evidence.source.as_ref() == Some(&read.source))
            .count(),
        1
    );
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.predicate != RelationPredicate::Calls)
    );
}

#[test]
fn eiffel_renamed_case_insensitive_and_feature_local_receivers_target_only_their_formal() {
    for (text, definition_name, spelling) in [
        (
            "class C feature run (renamed: ANY) do renamed.use end end\n",
            "renamed",
            "renamed",
        ),
        (
            "cLaSs C fEaTuRe run (VaLuE: ANY) do vAlUe.use end end\n",
            "VaLuE",
            "vAlUe",
        ),
        (
            "class C feature run (value: ANY) local other: ANY do value.use end other (value: ANY) do end end\n",
            "value",
            "value",
        ),
    ] {
        let definition = u64::try_from(text.find(definition_name).unwrap()).unwrap();
        let receiver = u64::try_from(text.find(&format!("{spelling}.use")).unwrap()).unwrap();
        let result = output(text.as_bytes());
        assert_receiver(
            &result,
            text.as_bytes(),
            definition_name,
            (
                definition,
                definition + u64::try_from(definition_name.len()).unwrap(),
            ),
            (receiver, receiver + u64::try_from(spelling.len()).unwrap()),
            spelling.as_bytes(),
        );
    }
}

#[test]
fn eiffel_ambiguous_alias_recovered_and_non_receiver_uses_remain_unresolved() {
    for text in [
        "class C feature run (value, VALUE: ANY) do value.use end end\n",
        "class C feature run (value: ANY) local VALUE: ANY do value.use end end\n",
        "class C feature run (value: ANY) do consume (agent do value.use end) end end\n",
        "class C feature run (value: ANY) do across items as item loop value.use end end end\n",
        "class C feature run (value: ANY) do if attached other as held then value.use end end end\n",
        "class C feature run (value: ANY) do separate other as held do value.use end end end\n",
        "class C feature run (value: ANY) do value.use ( end end\n",
        "class C feature run do value.use end end\n",
        "class C feature run (value: ANY) do consume (value) end end\n",
        "class C feature run (value: ANY) do Current.value.use end end\n",
        "class C feature run (value: ANY) do value (1).use end end\n",
        "class C feature run (value: ANY) do (value).use end end\n",
        "class C feature run (value: ANY) do consume (\"value.use\") -- value.use\nend end\n",
        "class C feature first, second (value: ANY) do value.use end end\n",
    ] {
        let result = output(text.as_bytes());
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .all(|site| site.role != OccurrenceRole::Read),
            "{text}"
        );
        assert!(
            result
                .document()
                .relations
                .iter()
                .all(|edge| edge.predicate != RelationPredicate::Calls),
            "{text}"
        );
    }
}

#[test]
fn eiffel_formal_receivers_preserve_utf8_bom_and_both_utf16_physical_frames() {
    let text = "class C\r\nfeature -- λ😀\r\n run (value: ANY) do value.use end\r\nend\r\n";
    let definition = text.find("value").unwrap();
    let receiver = text.rfind("value").unwrap();
    let utf8 = format!("\u{feff}{text}").into_bytes();
    let result = output(&utf8);
    assert_receiver(
        &result,
        &utf8,
        "value",
        (
            u64::try_from(definition + 3).unwrap(),
            u64::try_from(definition + 8).unwrap(),
        ),
        (
            u64::try_from(receiver + 3).unwrap(),
            u64::try_from(receiver + 8).unwrap(),
        ),
        b"value",
    );
    for little in [true, false] {
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
        let physical = |at| u64::try_from(2 + 2 * text[..at].encode_utf16().count()).unwrap();
        let result = output_encoding(
            &bytes,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        assert_receiver(
            &result,
            &bytes,
            "value",
            (physical(definition), physical(definition + 5)),
            (physical(receiver), physical(receiver + 5)),
            b"value",
        );
    }
}

#[test]
fn eiffel_native_and_artifact_required_counts_keep_both_receiver_witnesses_at_exact_fit() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, EIFFEL);
    let fixture = Fixture::new(EIFFEL, ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, EIFFEL, &budget);
    let (_, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let required = artifact.required_syntax_fact_count(&deadline()).unwrap();
    assert_eq!(
        required,
        native
            .required_syntax_fact_count(&input.to_parse_request(), &deadline())
            .unwrap()
    );
    let exact = limits_with_syntax_records(required);
    let (_, exact_artifact) = frontend
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, EIFFEL, &exact),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        exact_artifact
            .required_syntax_fact_count(&deadline())
            .unwrap(),
        required
    );
    let smaller = limits_with_syntax_records(required - 1);
    assert!(matches!(frontend.analyze_and_capture(
        &request(&fixture.snapshot, &fixture.source, EIFFEL, &smaller), ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()),
        Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }))
            if observed == required && limit == required - 1));
    let successor = fixture.next_generation();
    let next = request(&successor.snapshot, &successor.source, EIFFEL, &budget);
    let reused = frontend
        .analyze_from_artifact(
            &next,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let clean = analyze(&frontend, &next, &ExtensionSupport::default());
    assert_eq!(reused.document(), clean.document());
    assert_eq!(reused.report(), clean.report());
}

#[test]
fn eiffel_real_native_output_has_one_receiver_reference_and_one_endpoint_certificate() {
    let native = provider();
    let budget = limits();
    let reduced_start = u64::try_from(EIFFEL.source.rfind("value").unwrap()).unwrap();
    for (bytes, start, end) in [
        (EIFFEL.source.as_bytes(), reduced_start, reduced_start + 5),
        (ORIGINAL, 430, 440),
    ] {
        let fixture = Fixture::new(EIFFEL, bytes);
        let input = request(&fixture.snapshot, &fixture.source, EIFFEL, &budget);
        let parsed = rootlight_adapter_sdk::execute_parse(
            &native,
            &input.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let references = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Occurrence
                    && fact.syntax_kind().as_str() == "eiffel.identifier.reference"
                    && fact.span().start_byte() == start
                    && fact.span().end_byte() == end
            })
            .collect::<Vec<_>>();
        assert_eq!(references.len(), 1);
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| {
                    fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature
                        && fact.syntax_kind().as_str() == "eiffel.formal_receiver_target.signature"
                        && fact.span().end_byte() == end
                })
                .count(),
            1
        );
        let clean = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == "eiffel.formal_receiver_clean_file.signature"
            })
            .collect::<Vec<_>>();
        assert_eq!(clean.len(), 1);
        assert_eq!(clean[0].span(), fixture.source.span());
    }
}

#[test]
fn eiffel_formal_receiver_ir_reservations_keep_exact_fit_and_one_less_boundaries() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, EIFFEL);
    let fixture = Fixture::new(EIFFEL, EIFFEL.source.as_bytes());
    let base = limits();
    let run = |ir| {
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
        execute_analysis(
            &frontend,
            &request(&fixture.snapshot, &fixture.source, EIFFEL, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
    };
    let baseline = run(IrLimits::default()).unwrap();
    assert_eq!(
        baseline
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Read)
            .count(),
        1
    );
    for dimension in 0..4 {
        let make = |bound| {
            let mut ir = IrLimits::default();
            match dimension {
                0 => ir.max_occurrences = bound,
                1 => ir.max_relations = bound,
                2 => ir.max_total_records = bound,
                _ => ir.max_total_string_bytes = bound,
            }
            ir
        };
        let mut low = 0;
        let mut high = 16_384;
        while low < high {
            let middle = low + (high - low) / 2;
            match run(make(middle)) {
                Ok(result) => {
                    assert_eq!(result.document(), baseline.document());
                    high = middle;
                }
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("unexpected capacity result: {error:?}"),
            }
        }
        assert!(low > 0 && low < 16_384);
        assert_eq!(run(make(low)).unwrap().document(), baseline.document());
        assert!(
            matches!(run(make(low - 1)), Err(AdapterError::Sink(SinkError::StreamLimit { resource, observed, limit }))
            if observed > limit && limit == low - 1
                && resource == if dimension == 3 { ResourceKind::StringBytes } else { ResourceKind::Records })
        );
    }
}

#[test]
fn eiffel_read_metadata_preserves_written_identity_and_rejects_stale_or_cancelled_artifacts() {
    let initial = output(EIFFEL.source.as_bytes());
    let changed = EIFFEL.source.replace("value.run", "value.other");
    let updated = output(changed.as_bytes());
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
    assert_eq!(identities(&initial), identities(&updated));
    let native = Arc::new(provider());
    let frontend = analyzer(&native, EIFFEL);
    let fixture = Fixture::new(EIFFEL, EIFFEL.source.as_bytes());
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, EIFFEL, &budget);
    let artifact = frontend.prepare_artifact(&input, &deadline()).unwrap();
    let successor = fixture.rewrite(changed.as_bytes());
    let next = request(&successor.snapshot, &successor.source, EIFFEL, &budget);
    assert!(
        frontend
            .analyze_prepared_artifact(
                &next,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
    let cancelled = deadline();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        frontend.analyze_prepared_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}

#[test]
fn eiffel_complete_original_formal_receiver_retains_reference_and_independent_read() {
    assert_eq!(ORIGINAL.len(), 1177);
    assert!(ORIGINAL.starts_with(b"\xef\xbb\xbf"));
    assert_eq!(
        ORIGINAL.windows(2).filter(|pair| *pair == b"\r\n").count(),
        49
    );
    let result = output(ORIGINAL);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind, start, end) in [
        ("KL_BOOLEAN_ROUTINES", EntityKind::Class, 217, 236),
        ("nxor", EntityKind::Function, 262, 266),
        ("a_booleans", EntityKind::Parameter, 268, 278),
        ("i", EntityKind::Variable, 400, 401),
    ] {
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.source.span().start_byte() == start
                        && site.source.span().end_byte() == end
                        && document.entities.iter().any(|entity| {
                            entity.canonical_name == name
                                && entity.kind == kind
                                && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                        })
                })
                .count(),
            1,
            "{name}"
        );
    }
    let entity = |name: &str, kind: EntityKind| {
        let found: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect();
        assert_eq!(found.len(), 1, "unique current entity {name}");
        found[0]
    };
    let module = entity(EIFFEL.path, EntityKind::Module);
    let class = entity("KL_BOOLEAN_ROUTINES", EntityKind::Class);
    let routine = entity("nxor", EntityKind::Function);
    let parameter = entity("a_booleans", EntityKind::Parameter);
    let local = entity("i", EntityKind::Variable);
    let module_source = module.evidence.source.as_ref().unwrap();
    assert_eq!(
        module.container,
        Some(rootlight_ir::ContainerRef::File(
            module_source.span().file()
        ))
    );
    assert_eq!(
        (
            module_source.span().start_byte(),
            module_source.span().end_byte()
        ),
        (0, 1177)
    );
    assert!(document.files.iter().any(|file| file.byte_length == 1177));
    for (owner, child) in [
        (module, class),
        (class, routine),
        (routine, parameter),
        (routine, local),
    ] {
        assert_eq!(
            child.container,
            Some(rootlight_ir::ContainerRef::Entity(owner.id))
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(owner.id)
                        && edge.object == RelationEndpoint::Entity(child.id)
                })
                .count(),
            1,
            "exact written containment {} -> {}",
            owner.canonical_name,
            child.canonical_name
        );
    }
    for entity in [module, class, routine, parameter, local] {
        let source = entity.evidence.source.as_ref().unwrap();
        assert_eq!(entity.repository, document.repository);
        assert_eq!(entity.generation, document.generation);
        assert_eq!(entity.provenance, module.provenance);
        assert_eq!(source.repository(), document.repository);
        assert_eq!(source.generation(), document.generation);
        assert_eq!(source.content_hash(), content_hash(ORIGINAL));
        assert_eq!(source.span().file(), module_source.span().file());
    }
    let at_receiver = |site: &&rootlight_ir::OccurrenceRecord| {
        site.source.span().start_byte() == 430 && site.source.span().end_byte() == 440
    };
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(at_receiver)
        .filter(|site| site.role == OccurrenceRole::Reference)
        .collect();
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].syntax_kind, "eiffel.identifier.reference");
    assert!(matches!(
        references[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(references[0].enclosing, Some(routine.id));
    assert_eq!(references[0].provenance, module.provenance);
    assert_eq!(references[0].source.content_hash(), content_hash(ORIGINAL));
    assert_eq!(references[0].source.repository(), document.repository);
    assert_eq!(references[0].source.generation(), document.generation);
    assert_eq!(
        references[0].source.span().file(),
        module_source.span().file()
    );
    assert_eq!(
        references[0].evidence.source.as_ref(),
        Some(&references[0].source)
    );
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(at_receiver)
        .filter(|site| site.role == OccurrenceRole::Read)
        .collect();
    eprintln!(
        "complete_original_bytes={} exact_receiver=430..440 owner=nxor formal=268..278 current_ownership_join=true actual_reads={}",
        ORIGINAL.len(),
        reads.len()
    );
    assert_eq!(reads.len(), 1);
    let read = reads[0];
    assert_eq!(read.syntax_kind, "eiffel.formal_receiver.read");
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
    assert_eq!(read.syntactic_text_hash, content_hash(b"a_booleans"));
    assert_eq!(read.source.content_hash(), content_hash(ORIGINAL));
    assert_eq!(read.source.repository(), document.repository);
    assert_eq!(read.source.generation(), document.generation);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::RefersTo
                    && relation.subject == RelationEndpoint::Occurrence(read.id)
                    && relation.object == RelationEndpoint::Entity(parameter.id)
                    && relation.evidence.source.as_ref() == Some(&read.source)
            })
            .count(),
        1
    );
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate != RelationPredicate::Calls)
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}
