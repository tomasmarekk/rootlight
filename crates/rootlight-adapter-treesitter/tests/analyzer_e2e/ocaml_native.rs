//! Public OCaml analysis retains written implementation and interface identities.
//! Source evidence is generation-bound; calls and abstract types do not imply binding.

use super::*;

/// Written implementation with a container, callable and physical application.
pub(super) const OCAML: LanguageCase = LanguageCase {
    name: "ocaml",
    path: "src/written.ml",
    frontend: "tree-sitter-ocaml-0.25.0",
    source: "module Worker = struct\nlet increment x = x + 1\nlet result = increment 2\nend\n",
    generated: false,
    body_before: "x + 1",
    body_after: "x + 100",
};

/// Written interface declarations with an intentionally abstract type.
pub(super) const OCAML_INTERFACE: LanguageCase = LanguageCase {
    name: "ocaml-interface",
    path: "src/written.mli",
    frontend: "tree-sitter-ocaml-0.25.0",
    source: "type item\nmodule type Reader = sig\nval read : item -> int\nend\nmodule Worker : sig\nval run : item -> int\nend\nval increment : int -> int\nval count : int\n",
    generated: false,
    body_before: "int -> int",
    body_after: "int -> int -> int",
};

fn text<'a>(source: &'a str, reference: &SourceRef) -> &'a str {
    source
        .get(
            usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap(),
        )
        .unwrap()
}

fn output(case: LanguageCase, source: &str) -> AnalysisOutput {
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
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.language, case.name);
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    for reference in document
        .entities
        .iter()
        .filter_map(|entity| entity.evidence.source.as_ref())
        .chain(
            document
                .occurrences
                .iter()
                .map(|occurrence| &occurrence.source),
        )
        .chain(
            document
                .occurrences
                .iter()
                .filter_map(|occurrence| occurrence.evidence.source.as_ref()),
        )
        .chain(document.skipped_regions.iter().map(|gap| &gap.source))
        .chain(
            document
                .extensions
                .iter()
                .filter_map(|extension| extension.evidence.source.as_ref()),
        )
    {
        assert_eq!(reference.repository(), document.repository);
        assert_eq!(reference.generation(), document.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        let _ = text(source, reference);
    }
    for occurrence in &document.occurrences {
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(text(source, &occurrence.source).as_bytes())
        );
        let evidence = occurrence.evidence.source.as_ref().unwrap();
        assert!(evidence.span().start_byte() <= occurrence.source.span().start_byte());
        assert!(occurrence.source.span().end_byte() <= evidence.span().end_byte());
        if occurrence.role != OccurrenceRole::Definition {
            assert_eq!(evidence, &occurrence.source);
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
    }
    for coverage in &document.coverage_records {
        assert_eq!(
            coverage.discovered,
            coverage.indexed.checked_add(coverage.skipped).unwrap()
        );
    }
    result
}

fn entity<'a>(
    result: &'a AnalysisOutput,
    name: &str,
    kind: EntityKind,
) -> &'a rootlight_ir::EntityRecord {
    let mut matches = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name && entity.kind == kind);
    let entity = matches
        .next()
        .unwrap_or_else(|| panic!("missing {kind:?} {name}"));
    assert!(matches.next().is_none(), "duplicate {kind:?} {name}");
    entity
}

fn signature(result: &AnalysisOutput, source: &str, symbol: SymbolId) -> Vec<String> {
    result
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .filter_map(|extension| {
            let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
            (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                && lexical.subject() == rootlight_ir::FactRef::Entity(symbol))
            .then(|| {
                assert!(!lexical.is_truncated());
                lexical
                    .verify_source_bytes(
                        text(source, extension.evidence.source.as_ref().unwrap()).as_bytes(),
                    )
                    .unwrap();
                lexical.text().to_owned()
            })
        })
        .collect()
}

#[test]
fn ocaml_written_implementation_names_headers_and_containment_are_exact() {
    let result = output(OCAML, OCAML.source);
    let worker = entity(&result, "Worker", EntityKind::Module);
    let function = entity(&result, "increment", EntityKind::Function);
    let parameter = entity(&result, "x", EntityKind::Parameter);
    let variable = entity(&result, "result", EntityKind::Variable);
    for (owner, expected_name, expected_header) in [
        (worker, "Worker", "Worker ="),
        (function, "increment", "increment x ="),
        (parameter, "x", "x"),
        (variable, "result", "result ="),
    ] {
        let definitions = result.document().occurrences.iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::Definition
                && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == owner.id))
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{expected_name}");
        assert_eq!(text(OCAML.source, &definitions[0].source), expected_name);
        assert!(
            signature(&result, OCAML.source, owner.id)
                .iter()
                .any(|header| header == expected_header)
        );
    }
    for (parent, child) in [
        (worker, function),
        (function, parameter),
        (worker, variable),
    ] {
        assert!(
            result
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(parent.id)
                    && relation.object == RelationEndpoint::Entity(child.id))
        );
    }
}

#[test]
fn ocaml_applications_and_qualified_reads_remain_written_unresolved_uses() {
    let source =
        "let increment x = x + 1\nlet result = increment 2\nlet foreign item = Reader.read item\n";
    let result = output(OCAML, source);
    let mut calls = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .map(|occurrence| text(source, &occurrence.source))
        .collect::<Vec<_>>();
    calls.sort_unstable();
    assert_eq!(calls, ["Reader.read", "increment"]);
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Reference
                && text(source, &occurrence.source) == "item")
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::UnsupportedConstruct)
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn ocaml_interfaces_keep_signatures_and_abstract_type_source_without_implementations() {
    let result = output(OCAML_INTERFACE, OCAML_INTERFACE.source);
    for name in ["read", "run", "increment"] {
        let owner = entity(&result, name, EntityKind::Function);
        assert!(!signature(&result, OCAML_INTERFACE.source, owner.id).is_empty());
    }
    let _ = entity(&result, "count", EntityKind::Variable);
    let _ = entity(&result, "Worker", EntityKind::Module);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "item")
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| matches!(
                occurrence.role,
                OccurrenceRole::CallSite | OccurrenceRole::Reference
            ))
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(
                |gap| gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && text(OCAML_INTERFACE.source, &gap.source) == "item"
            )
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn ocaml_strings_comments_and_extension_payloads_cannot_fabricate_code_facts() {
    let source = "let visible x = x\nlet text = {aéz|let string_name x = x|aéz}\n(* let comment_name x = x *)\n[%%opaque let payload_name x = x]\n";
    let result = output(OCAML, source);
    let _ = entity(&result, "visible", EntityKind::Function);
    let _ = entity(&result, "text", EntityKind::Variable);
    for hidden in ["string_name", "comment_name", "payload_name"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == hidden)
        );
        assert!(!result.document().occurrences.iter().any(|occurrence| {
            !matches!(
                occurrence.role,
                OccurrenceRole::Documentation | OccurrenceRole::StringEvidence
            ) && text(source, &occurrence.source).contains(hidden)
        }));
    }
    for (role, authored) in [
        (
            OccurrenceRole::StringEvidence,
            "{aéz|let string_name x = x|aéz}",
        ),
        (
            OccurrenceRole::Documentation,
            "(* let comment_name x = x *)",
        ),
    ] {
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == role
                    && text(source, &occurrence.source) == authored)
        );
    }
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(
                |gap| gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && text(source, &gap.source) == "[%%opaque let payload_name x = x]"
            )
    );
}

#[test]
fn ocaml_body_only_changes_preserve_written_identity_and_lexical_headers() {
    let before = "let increment x = x + 1\n";
    let after = "let increment x = x + 100\n";
    let first = output(OCAML, before);
    let second = output(OCAML, after);
    for (name, kind) in [
        ("increment", EntityKind::Function),
        ("x", EntityKind::Parameter),
    ] {
        let first_owner = entity(&first, name, kind);
        let second_owner = entity(&second, name, kind);
        assert_eq!(first_owner.id, second_owner.id);
        assert_eq!(
            signature(&first, before, first_owner.id),
            signature(&second, after, second_owner.id)
        );
    }
}

#[test]
fn ocaml_concrete_types_keep_written_identity_and_narrow_abstract_gaps() {
    let source = "type point = { x: int; y: int }\ntype direction = North | South of int\ntype count = int\ntype abstract\ntype replacement := int\ntype extensible = ..\n";
    for case in [OCAML, OCAML_INTERFACE] {
        let result = output(case, source);
        for (name, kind) in [
            ("point", EntityKind::Struct),
            ("direction", EntityKind::Enum),
            ("count", EntityKind::TypeAlias),
        ] {
            let owner = entity(&result, name, kind);
            let definitions = result.document().occurrences.iter().filter(|occurrence|
                occurrence.role == OccurrenceRole::Definition
                && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == owner.id)
            ).collect::<Vec<_>>();
            assert_eq!(definitions.len(), 1);
            assert_eq!(text(source, &definitions[0].source), name);
            assert!(!signature(&result, source, owner.id).is_empty());
        }
        for name in ["abstract", "replacement", "extensible"] {
            assert!(
                !result
                    .document()
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == name)
            );
            assert!(
                result
                    .document()
                    .skipped_regions
                    .iter()
                    .any(
                        |gap| gap.reason == SkippedRegionReason::UnsupportedConstruct
                            && text(source, &gap.source).starts_with(name)
                    )
            );
        }
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls)
        );
    }
}

fn written_output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let parser = Arc::new(provider());
    let fixture = Fixture::new(OCAML, bytes);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(OCAML),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    analyze(
        &analyzer(&parser, OCAML),
        &request,
        &ExtensionSupport::default(),
    )
}

fn written_site(
    document: &rootlight_ir::NormalizedIrDocument,
    start: u64,
    end: u64,
    role: OccurrenceRole,
) -> &rootlight_ir::OccurrenceRecord {
    let matches = document
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
                && site.role == role
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "exact written occurrence {start}..{end}");
    matches[0]
}

fn written_definition(
    document: &rootlight_ir::NormalizedIrDocument,
    start: u64,
    end: u64,
    kind: EntityKind,
) -> &rootlight_ir::EntityRecord {
    let definition = written_site(document, start, end, OccurrenceRole::Definition);
    let OccurrenceTarget::Resolved { symbol } = definition.target else {
        panic!("own Definition prerequisite")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    entity
}

fn assert_written_bindings(
    document: &rootlight_ir::NormalizedIrDocument,
    raw: &[u8],
    offset: impl Fn(usize) -> u64,
) {
    let parameter = written_definition(document, offset(64), offset(65), EntityKind::Parameter);
    let target = written_definition(document, offset(37), offset(63), EntityKind::Function);
    let caller = written_definition(document, offset(217), offset(239), EntityKind::Function);
    assert!(
        document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(target.id)
                && relation.object == RelationEndpoint::Entity(parameter.id))
    );
    let reference = written_site(document, offset(81), offset(82), OccurrenceRole::Reference);
    assert_eq!(
        reference.target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
    assert_eq!(reference.syntax_kind, "ocaml.match_formal.reference");
    assert_eq!(reference.enclosing, Some(target.id));
    let call = written_site(document, offset(244), offset(270), OccurrenceRole::CallSite);
    assert_eq!(
        call.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert_eq!(call.syntax_kind, "ocaml.direct_file_call.call");
    assert_eq!(call.enclosing, Some(caller.id));
    let relations = document
        .relations
        .iter()
        .filter(|relation| relation.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(relations.len(), 1);
    let relation = relations[0];
    assert_eq!(relation.subject, RelationEndpoint::Entity(caller.id));
    assert_eq!(relation.object, RelationEndpoint::Entity(target.id));
    assert_eq!(relation.evidence.source.as_ref(), Some(&call.source));
    let expected = [
        rootlight_ir::FactRef::Entity(caller.id),
        rootlight_ir::FactRef::Fact(call.id),
        rootlight_ir::FactRef::Entity(target.id),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(
        relation
            .evidence
            .derivation
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        expected
    );
    assert_eq!(relation.evidence.derivation.len(), 3);
    for site in [reference, call] {
        assert_eq!(site.repository, document.repository);
        assert_eq!(site.generation, document.generation);
        assert_eq!(site.file, document.files[0].id);
        assert_eq!(site.source.content_hash(), content_hash(raw));
        assert_eq!(site.source.span().file(), site.file);
        assert_eq!(site.evidence.source.as_ref(), Some(&site.source));
        assert_eq!(site.provenance, target.provenance);
    }
    assert_eq!(
        document.files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
    assert_eq!(document.files[0].content_hash, content_hash(raw));
}

#[test]
fn ocaml_original_match_formal_and_preceding_written_function_have_exact_targets() {
    let raw = include_bytes!("../fixtures/ocaml_written/caml_bytestring.ml");
    assert_eq!(raw.len(), 899);
    assert_eq!(&raw[64..65], b"s");
    assert_eq!(&raw[81..82], b"s");
    assert_eq!(&raw[244..270], b"caml_bytestring_length_aux");
    let result = written_output(raw, EncodingId::utf8());
    assert_written_bindings(result.document(), raw, |offset| {
        u64::try_from(offset).unwrap()
    });
    for (start, end, role) in [
        (170, 196, OccurrenceRole::CallSite),
        (200, 204, OccurrenceRole::CallSite),
        (271, 272, OccurrenceRole::Reference),
    ] {
        assert!(matches!(
            written_site(result.document(), start, end, role).target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Relations)
    );
}

#[test]
fn ocaml_written_binding_boundaries_keep_unproved_calls_and_patterns_unresolved() {
    for source in [
        "let target value = value\nlet caller value = Other.target value\n",
        "let target value = value\nopen Other\nlet caller value = target value\n",
        "let target value = value\ninclude Other\nlet caller value = target value\n",
        "let target value = value\nlet caller target = target 1\n",
        "let target ~value = value\nlet caller value = target value\n",
        "let target ?(value=0) other = value\nlet caller value = target value\n",
        "let target a b = a\nlet caller value = target value\n",
        "let target value = value\nlet caller value = let target = other in target value\n",
        "let target value = value and hidden x = x\nlet caller value = target value\n",
        "let target value = value\nlet result = target 1\n",
        "let target value = value\nlet caller value = target (fun value -> value)\n",
        "let target value = value\nlet caller value = target value [@@opaque]\n",
        "let target value = value\nlet caller value = target value\nlet broken = (\n",
        "let choose Some = match Some with Some -> 1\n",
    ] {
        let result = written_output(source.as_bytes(), EncodingId::utf8());
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::Calls),
            "{source}"
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .filter(|s| s.role != OccurrenceRole::Definition)
                .all(|s| !matches!(s.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
        assert_eq!(
            result.document().files[0].content_hash,
            content_hash(source.as_bytes())
        );
    }
    let source = "let target value = value\nlet caller value = target value\n";
    let result = written_output(source.as_bytes(), EncodingId::utf8());
    assert_eq!(
        result
            .document()
            .relations
            .iter()
            .filter(|r| r.predicate == RelationPredicate::Calls)
            .count(),
        1
    );
    let source = "let f value = match value with Some value -> value | None -> 0\n";
    let result = written_output(source.as_bytes(), EncodingId::utf8());
    let start = source.find("match value").unwrap() + 6;
    let reference = written_site(
        result.document(),
        u64::try_from(start).unwrap(),
        u64::try_from(start + 5).unwrap(),
        OccurrenceRole::Reference,
    );
    assert!(matches!(
        reference.target,
        OccurrenceTarget::Resolved { .. }
    ));
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|s| s.role == OccurrenceRole::Reference
                && s.source.span().start_byte() != u64::try_from(start).unwrap())
            .all(|s| matches!(s.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn ocaml_written_certificates_keep_both_utf16_orders_and_physical_definitions() {
    let text = std::str::from_utf8(include_bytes!(
        "../fixtures/ocaml_written/caml_bytestring.ml"
    ))
    .unwrap();
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            raw.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let result = written_output(
            &raw,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        assert_written_bindings(result.document(), &raw, |offset| {
            2 + u64::try_from(text[..offset].encode_utf16().count() * 2).unwrap()
        });
    }
}

#[test]
fn ocaml_required_native_proofs_artifact_counts_exact_capacity_and_replay_agree() {
    let raw = include_bytes!("../fixtures/ocaml_written/caml_bytestring.ml");
    let parser = Arc::new(provider());
    let analyzer = analyzer(&parser, OCAML);
    let fixture = Fixture::new(OCAML, raw);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, OCAML, &budget);
    let required = parser
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 1);
    let artifact = analyzer.prepare_artifact(&initial, &deadline()).unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let parsed = rootlight_adapter_sdk::execute_parse(
        &*parser,
        &initial.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    for label in [
        "ocaml.match_formal.reference",
        "ocaml.direct_file_call.call",
        "ocaml.match_formal_target.signature",
        "ocaml.direct_file_target.signature",
        "ocaml.written_bindings_file.signature",
    ] {
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|f| f.syntax_kind().as_str() == label)
                .count(),
            1,
            "{label}"
        );
    }
    let exact = limits_with_syntax_records(required);
    let exact_request = request(&fixture.snapshot, &fixture.source, OCAML, &exact);
    let (result, captured) = analyzer
        .analyze_and_capture(
            &exact_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_written_bindings(result.document(), raw, |b| u64::try_from(b).unwrap());
    assert_eq!(
        captured.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let short = limits_with_syntax_records(required - 1);
    let error = analyzer
        .prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, OCAML, &short),
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(error,AdapterError::Sink(SinkError::StreamLimit{resource:ResourceKind::RequiredSyntaxFacts,observed,limit})
        if observed == required && limit == required-1),
        "{error:?}"
    );
    let successor = fixture.next_generation();
    let successor_request = request(&successor.snapshot, &successor.source, OCAML, &exact);
    let reused = analyzer
        .analyze_from_artifact(
            &successor_request,
            &captured,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &successor_request, &ExtensionSupport::default());
    assert_eq!(reused.document(), fresh.document());
    assert_eq!(reused.report(), fresh.report());
    assert_written_bindings(reused.document(), raw, |b| u64::try_from(b).unwrap());
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyzer.analyze_from_artifact(
            &successor_request,
            &captured,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled
        ),
        Err(AdapterError::Cancelled { .. })
    ));
    let changed = fixture.rewrite(&[raw.as_slice(), b"\n(* changed *)\n"].concat());
    assert!(
        analyzer
            .analyze_from_artifact(
                &request(&changed.snapshot, &changed.source, OCAML, &exact),
                &captured,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
}

#[test]
fn ocaml_written_raw_witness_tampering_cannot_reuse_materialized_targets() {
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};
    let raw = include_bytes!("../fixtures/ocaml_written/caml_bytestring.ml");
    let native = provider();
    let fixture = Fixture::new(OCAML, raw);
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, OCAML, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts: Vec<SyntaxFact>| {
        let provider = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(provider),
            producer_identity(),
            language(OCAML),
            OCAML.frontend,
            content_hash(BINARY_SEED),
        )
        .unwrap();
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap()
    };
    let baseline = replay(parsed.facts().to_vec());
    assert_written_bindings(baseline.document(), raw, |b| u64::try_from(b).unwrap());
    for (label, start) in [
        ("ocaml.parameter.declaration", 64),
        ("ocaml.function.declaration", 37),
        ("ocaml.function.declaration", 217),
    ] {
        let declaration = parsed
            .facts()
            .iter()
            .find(|f| {
                f.kind() == SyntaxFactKind::Declaration
                    && f.syntax_kind().as_str() == label
                    && f.span().start_byte() == start
            })
            .unwrap();
        let definition = parsed
            .facts()
            .iter()
            .find(|f| {
                f.syntax_kind().as_str() == "ocaml.name.definition"
                    && f.parent() == Some(declaration.local_id())
            })
            .unwrap();
        for witness in [declaration, definition] {
            let mut facts = parsed.facts().to_vec();
            let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
            facts.push(SyntaxFact::new(
                next,
                witness.parent(),
                witness.kind(),
                witness.span(),
                witness.depth(),
                witness.syntax_kind().clone(),
            ));
            let changed = replay(facts);
            assert_eq!(
                changed
                    .document()
                    .entities
                    .iter()
                    .map(|e| e.id)
                    .collect::<BTreeSet<_>>(),
                baseline
                    .document()
                    .entities
                    .iter()
                    .map(|e| e.id)
                    .collect::<BTreeSet<_>>(),
                "distinct-ID duplicate must exercise the retained materialized endpoints"
            );
            for (site_start, site_end, role) in if start == 64 {
                vec![(81, 82, OccurrenceRole::Reference)]
            } else {
                vec![(244, 270, OccurrenceRole::CallSite)]
            } {
                let site = written_site(changed.document(), site_start, site_end, role);
                assert!(
                    matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                    "distinct-ID {label} duplicate must poison own join"
                );
                assert!(
                    changed
                        .document()
                        .skipped_regions
                        .iter()
                        .any(|g| g.source == site.source && g.domain == FactDomain::Relations)
                );
            }
        }
        let mut facts = parsed.facts().to_vec();
        facts.retain(|f| f.local_id() != declaration.local_id());
        for fact in &mut facts {
            if fact.parent() == Some(declaration.local_id()) {
                *fact = SyntaxFact::new(
                    fact.local_id(),
                    declaration.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                );
            }
        }
        let changed = replay(facts);
        let (a, b, role) = if start == 64 {
            (81, 82, OccurrenceRole::Reference)
        } else {
            (244, 270, OccurrenceRole::CallSite)
        };
        assert!(matches!(
            written_site(changed.document(), a, b, role).target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
    for label in [
        "ocaml.written_bindings_file.signature",
        "ocaml.match_formal_target.signature",
        "ocaml.direct_file_target.signature",
    ] {
        let mut facts = parsed.facts().to_vec();
        facts.retain(|f| f.syntax_kind().as_str() != label);
        let changed = replay(facts);
        for site in changed.document().occurrences.iter().filter(|s| {
            s.syntax_kind == "ocaml.match_formal.reference"
                || s.syntax_kind == "ocaml.direct_file_call.call"
        }) {
            if label == "ocaml.written_bindings_file.signature"
                || label == "ocaml.match_formal_target.signature"
                    && site.role == OccurrenceRole::Reference
                || label == "ocaml.direct_file_target.signature"
                    && site.role == OccurrenceRole::CallSite
            {
                assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            }
        }
    }
}

#[test]
fn ocaml_written_call_ir_limits_refuse_typed_one_less_without_losing_source_proof() {
    let raw = include_bytes!("../fixtures/ocaml_written/caml_bytestring.ml");
    let parser = Arc::new(provider());
    let analyzer = analyzer(&parser, OCAML);
    let fixture = Fixture::new(OCAML, raw);
    let defaults = limits();
    let baseline = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, OCAML, &defaults),
        &ExtensionSupport::default(),
    );
    assert_written_bindings(baseline.document(), raw, |b| u64::try_from(b).unwrap());
    let run = |ir: IrLimits| {
        let budget = AnalysisLimits::new(
            defaults.max_source_bytes(),
            defaults.max_syntax_nodes(),
            defaults.max_syntax_depth(),
            defaults.max_embedded_ranges(),
            defaults.max_reported_memory_bytes(),
            defaults.syntax_stream().clone(),
            defaults.ir_stream().clone(),
            ir,
        )
        .unwrap();
        execute_analysis(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, OCAML, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
    };
    let mut small = defaults.ir().clone();
    small.max_nested_items_per_record = 2;
    let error = run(small).unwrap_err();
    assert!(
        matches!(
            error,
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::NestedItems,
                observed: 3,
                limit: 2
            })
        ),
        "{error:?}"
    );
    for dimension in [0, 1, 2] {
        let resource = match dimension {
            0 => ResourceKind::Records,
            1 => ResourceKind::Records,
            _ => ResourceKind::NestedItems,
        };
        let make = |bound| {
            let mut ir = defaults.ir().clone();
            match dimension {
                0 => ir.max_relations = bound,
                1 => ir.max_total_records = bound,
                _ => ir.max_total_nested_items = bound,
            };
            ir
        };
        let mut low = 0;
        let mut high = 65_536;
        assert_eq!(run(make(high)).unwrap().document(), baseline.document());
        while low < high {
            let middle = low + (high - low) / 2;
            match run(make(middle)) {
                Ok(result) => {
                    assert_eq!(result.document(), baseline.document());
                    high = middle;
                }
                Err(error) => {
                    assert!(
                        matches!(error,AdapterError::Sink(SinkError::StreamLimit{resource:r,..}) if r==resource),
                        "{error:?}"
                    );
                    low = middle + 1;
                }
            }
        }
        assert!(low > 0);
        assert_eq!(run(make(low)).unwrap().document(), baseline.document());
        let error = run(make(low - 1)).unwrap_err();
        assert!(
            matches!(error,AdapterError::Sink(SinkError::StreamLimit{resource:r,observed,limit})
            if r==resource && observed>limit && limit==low-1),
            "{error:?}"
        );
    }
}

#[test]
fn ocaml_binding_metadata_keeps_body_only_written_symbol_identity_stable() {
    let before = "let first value = match value with None -> 0 | Some item -> item\nlet second input = first input\n";
    let after = before.replace("None -> 0", "None -> 100");
    let first = written_output(before.as_bytes(), EncodingId::utf8());
    let second = written_output(after.as_bytes(), EncodingId::utf8());
    for (name, kind) in [
        ("first", EntityKind::Function),
        ("second", EntityKind::Function),
        ("value", EntityKind::Parameter),
        ("input", EntityKind::Parameter),
    ] {
        let a = entity(&first, name, kind);
        let b = entity(&second, name, kind);
        assert_eq!(a.id, b.id, "{name}");
        assert_eq!(
            signature(&first, before, a.id),
            signature(&second, &after, b.id)
        );
    }
    for output in [&first, &second] {
        assert_eq!(
            output
                .document()
                .relations
                .iter()
                .filter(|r| r.predicate == RelationPredicate::Calls)
                .count(),
            1
        );
        assert_eq!(
            output
                .document()
                .occurrences
                .iter()
                .filter(|s| s.syntax_kind == "ocaml.match_formal.reference"
                    && matches!(s.target, OccurrenceTarget::Resolved { .. }))
                .count(),
            1
        );
    }
}
