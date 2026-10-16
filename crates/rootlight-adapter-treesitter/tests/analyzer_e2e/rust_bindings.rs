//! Rust binding contracts distinguish written leaves from lexical visibility.
//! Physical definitions and owners are checked independently of reference targets.

use super::*;

const RUST: LanguageCase = LanguageCase {
    name: "rust",
    path: "src/bindings.rs",
    frontend: "tree-sitter-rust-0.24.2",
    source: "fn owner() { let value = 1; value; }\n",
    generated: false,
    body_before: "= 1",
    body_after: "= 2",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(RUST, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, RUST),
        &request(&fixture.snapshot, &fixture.source, RUST, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    for occurrence in &result.document().occurrences {
        let span = occurrence.source.span();
        let bytes = &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(bytes),
            "{source}: {} {span:?}",
            occurrence.syntax_kind
        );
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(source.as_bytes())
        );
    }
    result
}

fn site(source: &str, anchor: &str, name: &str) -> (u64, u64) {
    let start = source.find(anchor).expect("unique fixture anchor")
        + anchor.find(name).expect("name inside anchor");
    (
        u64::try_from(start).unwrap(),
        u64::try_from(start + name.len()).unwrap(),
    )
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    source: &str,
    anchor: &str,
    name: &str,
) -> SymbolId {
    let (start, end) = site(source, anchor, name);
    let matches: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.source.span().start_byte() == start
                && occurrence.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(matches.len(), 1, "missing exact definition {anchor}");
    let OccurrenceTarget::Resolved { symbol } = matches[0].target else {
        panic!("written definition has no entity: {anchor}");
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.canonical_name, name);
    symbol
}

fn reference(
    document: &rootlight_ir::NormalizedIrDocument,
    source: &str,
    anchor: &str,
    name: &str,
    expected: Option<SymbolId>,
) {
    let start = source.find(anchor).expect("unique fixture anchor")
        + anchor
            .rfind(name)
            .expect("last name occurrence inside anchor");
    let (start, end) = (
        u64::try_from(start).unwrap(),
        u64::try_from(start + name.len()).unwrap(),
    );
    let matches: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Reference
                && occurrence.source.span().start_byte() == start
                && occurrence.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(matches.len(), 1, "missing exact value reference {anchor}");
    match expected {
        Some(symbol) => {
            assert_eq!(
                matches[0].target,
                OccurrenceTarget::Resolved { symbol },
                "{anchor}"
            );
            assert!(document.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::RefersTo
                    && relation.subject == RelationEndpoint::Occurrence(matches[0].id)
                    && relation.object == RelationEndpoint::Entity(symbol)
                    && relation.evidence.source.as_ref() == Some(&matches[0].source)
            }));
        }
        None => assert!(
            matches!(matches[0].target, OccurrenceTarget::Unresolved { .. }),
            "{anchor}"
        ),
    }
}

#[test]
fn rust_written_pattern_leaves_have_exact_definitions_and_owners() {
    let source = "struct Pair { left: i32, right: i32 }\nfn owner(input: Pair) {\n let simple = 1;\n let (mut first, (_, ref second)) = (1, (2, 3));\n let [head, tail @ ..] = [1, 2, 3];\n let Pair { left, right: renamed } = input;\n let Some(inner) = Some(1) else { return };\n let &borrowed = &1;\n let _ = 0;\n let _kept: i32;\n let ref mut changed = 2;\n}\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let owner = definition(document, source, "owner(input", "owner");
    let mut symbols = BTreeSet::new();
    for (anchor, name) in [
        ("simple =", "simple"),
        ("mut first", "first"),
        ("ref second", "second"),
        ("[head", "head"),
        ("tail @", "tail"),
        ("{ left,", "left"),
        ("right: renamed", "renamed"),
        ("Some(inner)", "inner"),
        ("&borrowed =", "borrowed"),
        ("_kept:", "_kept"),
        ("mut changed", "changed"),
    ] {
        let symbol = definition(document, source, anchor, name);
        assert!(symbols.insert(symbol));
        assert_eq!(
            document
                .entities
                .iter()
                .find(|entity| entity.id == symbol)
                .unwrap()
                .kind,
            EntityKind::Variable
        );
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner)
                    && relation.object == RelationEndpoint::Entity(symbol)),
            "wrong owner: {anchor}"
        );
    }
    for (anchor, name) in [
        ("(_, ref", "_"),
        ("let _ =", "_"),
        ("let Pair", "Pair"),
        ("let Some", "Some"),
        ("right: renamed", "right"),
    ] {
        let (start, end) = site(source, anchor, name);
        assert!(
            !document
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                    && occurrence.source.span().start_byte() == start
                    && occurrence.source.span().end_byte() == end),
            "non-binding defined: {anchor}"
        );
    }
}

#[test]
fn rust_local_activation_shadowing_and_sibling_scopes_do_not_cross() {
    let source = "fn owner() { before; let value = 1; let value = value + 1; after(value); { let sibling = 1; use_left(sibling); } { use_right(sibling); } let own = own; use_own(own); }";
    let result = output(source);
    let document = result.document();
    let first = definition(document, source, "value = 1", "value");
    let second = definition(document, source, "value = value", "value");
    assert_ne!(first, second);
    reference(document, source, "value + 1", "value", Some(first));
    reference(document, source, "after(value)", "value", Some(second));
    let sibling = definition(document, source, "sibling = 1", "sibling");
    reference(
        document,
        source,
        "use_left(sibling)",
        "sibling",
        Some(sibling),
    );
    reference(document, source, "use_right(sibling)", "sibling", None);
    reference(document, source, "before;", "before", None);
    reference(document, source, "= own;", "own", None);
    let own = definition(document, source, "own = own", "own");
    reference(document, source, "use_own(own)", "own", Some(own));
}

#[test]
fn rust_let_else_uses_the_prior_environment_until_the_whole_declaration_ends() {
    let source = "fn owner() { let value = 7; let Some(value) = produce(value) else { inspect(value); return }; after(value); }";
    let result = output(source);
    let document = result.document();
    let first = definition(document, source, "value = 7", "value");
    let second = definition(document, source, "Some(value)", "value");
    reference(document, source, "produce(value)", "value", Some(first));
    reference(document, source, "inspect(value)", "value", Some(first));
    reference(document, source, "after(value)", "value", Some(second));
}

#[test]
fn rust_closures_keep_creation_environment_parameters_and_written_ownership() {
    let source = "fn owner(parameter: i32) { let value = 1; let capture = || { let inner = value; use_inner(inner); }; let value = 2; let shadow = |parameter| { use_parameter(parameter); }; fn nested() { use_nested(value); } use_outer(parameter); }";
    let result = output(source);
    let document = result.document();
    let first = definition(document, source, "value = 1", "value");
    reference(document, source, "= value;", "value", Some(first));
    let outer = definition(document, source, "parameter: i32", "parameter");
    let parameter = definition(document, source, "|parameter|", "parameter");
    reference(
        document,
        source,
        "use_parameter(parameter)",
        "parameter",
        Some(parameter),
    );
    reference(
        document,
        source,
        "use_outer(parameter)",
        "parameter",
        Some(outer),
    );
    reference(document, source, "use_nested(value)", "value", None);
    let inner = definition(document, source, "inner = value", "inner");
    let closure = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Function
                && entity.canonical_name == "<anonymous>"
                && entity.evidence.source.as_ref().is_some_and(|reference| {
                    reference.span().start_byte()
                        == u64::try_from(source.find("||").unwrap()).unwrap()
                })
        })
        .expect("physical closure owner");
    assert!(
        document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(closure.id)
                && relation.object == RelationEndpoint::Entity(inner))
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol: closure.id })
    );
}

#[test]
fn rust_constant_patterns_and_macro_tokens_do_not_invent_local_definitions() {
    let source = "const LIMIT: i32 = 1; struct Unit; fn owner(input: i32) { let LIMIT = input else { return }; let Unit = Unit; let real = 2; use_real(real); opaque!(let invented = real); let generated!() = input; after_macro(real); }";
    let result = output(source);
    let document = result.document();
    let real = definition(document, source, "real = 2", "real");
    reference(document, source, "use_real(real)", "real", Some(real));
    reference(document, source, "after_macro(real)", "real", None);
    for (anchor, name) in [
        ("let LIMIT", "LIMIT"),
        ("let Unit", "Unit"),
        ("let invented", "invented"),
    ] {
        let (start, end) = site(source, anchor, name);
        assert!(
            !document
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                    && occurrence.source.span().start_byte() == start
                    && occurrence.source.span().end_byte() == end)
        );
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "rust-pattern-binding-namespace-unavailable")
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "rust-macro-expansion-bindings-unavailable")
    );
}

#[test]
fn rust_type_positions_and_local_calls_do_not_claim_value_or_dispatch_targets() {
    let source = "type Name = i32; fn callable() {} fn owner() { let Name = 1; let typed: Name = 2; let callable = || 1; callable(); use_name(Name); }";
    let result = output(source);
    let document = result.document();
    let value = definition(document, source, "Name = 1", "Name");
    reference(document, source, "use_name(Name)", "Name", Some(value));
    let (start, end) = site(source, "typed: Name", "Name");
    let type_use = document
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::TypeUse
                && occurrence.source.span().start_byte() == start
                && occurrence.source.span().end_byte() == end
        })
        .expect("type namespace occurrence");
    assert_ne!(
        type_use.target,
        OccurrenceTarget::Resolved { symbol: value }
    );
    let (start, end) = site(source, "callable();", "callable");
    let call = document
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.source.span().start_byte() == start
                && occurrence.source.span().end_byte() == end
        })
        .expect("written local call");
    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "rust-call-dispatch-target-unavailable")
    );
}

#[test]
fn rust_repeated_local_identities_survive_body_and_trivia_edits() {
    let source = "fn owner() { let value = 1; { let value = 2; use_it(value); } let value = 3; use_final(value); }";
    let original = output(source);
    let ids = ["value = 1", "value = 2", "value = 3"]
        .map(|anchor| definition(original.document(), source, anchor, "value"));
    assert_eq!(ids.into_iter().collect::<BTreeSet<_>>().len(), 3);
    let changed = source
        .replace("fn owner", "// unrelated trivia\nfn owner")
        .replace("= 2", "= 20")
        .replace("use_it(value)", "use_it( value )");
    let after = output(&changed);
    assert_eq!(
        ids,
        ["value = 1", "value = 20", "value = 3"].map(|anchor| definition(
            after.document(),
            &changed,
            anchor,
            "value"
        ))
    );
}

#[test]
fn rust_raw_and_unicode_bindings_preserve_utf16_physical_evidence() {
    let source =
        "fn owner(λ: i32) { let r#type = λ; inspect(r#type); let e\u{301} = λ; inspect(é); }";
    let mut encodings = vec![(source.as_bytes().to_vec(), None)];
    for little_endian in [true, false] {
        let mut bytes = if little_endian {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in source.encode_utf16() {
            bytes.extend(if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        encodings.push((bytes, Some(little_endian)));
    }
    let mut original_ids = None;
    for (bytes, encoding) in encodings {
        let provider = Arc::new(provider());
        let fixture = Fixture::new(RUST, &bytes);
        let budget = limits();
        let encoding_id = EncodingId::new(match encoding {
            None => "utf-8",
            Some(true) => "utf-16le",
            Some(false) => "utf-16be",
        })
        .unwrap();
        let encoded_request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(RUST),
            encoding_id,
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, RUST),
            &encoded_request,
            &ExtensionSupport::default(),
        );
        let document = result.document();
        validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
        assert!(
            document.diagnostics.is_empty(),
            "{:?}",
            document.diagnostics
        );
        let mut ids = Vec::new();
        for (anchor, written, canonical) in [
            ("λ: i32", "λ", "λ"),
            ("r#type =", "r#type", "type"),
            ("e\u{301} =", "e\u{301}", "é"),
        ] {
            let start = source.find(anchor).unwrap();
            let end = start + written.len();
            let physical = |offset: usize| {
                if encoding.is_some() {
                    2 + source[..offset].encode_utf16().count() * 2
                } else {
                    offset
                }
            };
            let occurrence = document
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.role == OccurrenceRole::Definition
                        && occurrence.source.span().start_byte()
                            == u64::try_from(physical(start)).unwrap()
                        && occurrence.source.span().end_byte()
                            == u64::try_from(physical(end)).unwrap()
                })
                .unwrap();
            let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
                panic!("physical definition")
            };
            assert_eq!(
                document
                    .entities
                    .iter()
                    .find(|entity| entity.id == symbol)
                    .unwrap()
                    .canonical_name,
                canonical
            );
            ids.push(symbol);
        }
        for occurrence in &document.occurrences {
            let span = occurrence.source.span();
            let physical_bytes = &bytes[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            let decoded = match encoding {
                None => String::from_utf8(physical_bytes.to_vec()).unwrap(),
                Some(little_endian) => String::from_utf16(
                    &physical_bytes
                        .chunks_exact(2)
                        .map(|pair| {
                            let pair = [pair[0], pair[1]];
                            if little_endian {
                                u16::from_le_bytes(pair)
                            } else {
                                u16::from_be_bytes(pair)
                            }
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap(),
            };
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(decoded.as_bytes())
            );
            assert_eq!(occurrence.source.content_hash(), content_hash(&bytes));
        }
        for (anchor, name, expected) in [
            ("inspect(r#type)", "r#type", ids[1]),
            ("inspect(é)", "é", ids[2]),
        ] {
            let start = source.find(anchor).unwrap() + anchor.rfind(name).unwrap();
            let physical = if encoding.is_some() {
                2 + source[..start].encode_utf16().count() * 2
            } else {
                start
            };
            let occurrence = document
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.role == OccurrenceRole::Reference
                        && occurrence.source.span().start_byte() == u64::try_from(physical).unwrap()
                })
                .unwrap();
            assert_eq!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol: expected }
            );
        }
        if let Some(original) = &original_ids {
            assert_eq!(original, &ids);
        } else {
            original_ids = Some(ids);
        }
    }
}

#[test]
fn rust_unavailable_patterns_block_outer_targets_only_in_their_visibility_regions() {
    let source = "fn owner() { let value = 1; let other = 2; for value in init_for(value) { body_for(value); unaffected_for(other); let value = 3; rebound_for(value); } after_for(value); if let Some(value) = init_if(value) { body_if(value); unaffected_if(other); } else { else_if(value); } after_if(value); while let Some(value) = init_while(value) { body_while(value); unaffected_while(other); } after_while(value); match init_match(value) { Some(value) => { body_match(value); unaffected_match(other); }, _ => other_arm(value) } after_match(value); { let (Some(value) | Other(value)) = init_or(value) else { return }; body_or(value); unaffected_or(other); let value = 4; rebound_or(value); } after_or(value); }";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let original = definition(document, source, "value = 1", "value");
    for (declaration, read) in [
        ("value = 3", "rebound_for(value)"),
        ("value = 4", "rebound_or(value)"),
    ] {
        let known = definition(document, source, declaration, "value");
        reference(document, source, read, "value", Some(known));
    }

    let other = definition(document, source, "other = 2", "other");
    for anchor in [
        "unaffected_for(other)",
        "unaffected_if(other)",
        "unaffected_while(other)",
        "unaffected_match(other)",
        "unaffected_or(other)",
    ] {
        reference(document, source, anchor, "other", Some(other));
    }

    for anchor in [
        "init_for(value)",
        "after_for(value)",
        "init_if(value)",
        "else_if(value)",
        "after_if(value)",
        "init_while(value)",
        "after_while(value)",
        "init_match(value)",
        "other_arm(value)",
        "after_match(value)",
        "init_or(value)",
        "after_or(value)",
    ] {
        reference(document, source, anchor, "value", Some(original));
    }
    for anchor in [
        "body_for(value)",
        "body_if(value)",
        "body_while(value)",
        "body_match(value)",
        "body_or(value)",
    ] {
        reference(document, source, anchor, "value", None);
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "rust-pattern-binding-namespace-unavailable")
    );
}

#[test]
fn rust_raw_and_normalized_constant_patterns_do_not_create_local_bindings() {
    let source = "const r#limit: i32 = 1; const e\u{301}: i32 = 2; fn owner() { let limit = 1 else { return }; let é = 2 else { return }; }";
    let result = output(source);
    let document = result.document();
    for (anchor, name) in [("let limit", "limit"), ("let é", "é")] {
        let (start, end) = site(source, anchor, name);
        assert!(
            !document
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                    && occurrence.source.span().start_byte() == start
                    && occurrence.source.span().end_byte() == end)
        );
        assert!(document.skipped_regions.iter().any(|gap| gap.detail
            == "rust-pattern-binding-namespace-unavailable"
            && gap.source.span().start_byte() == start
            && gap.source.span().end_byte() == end));
    }
}

fn direct_proofs(result: &AnalysisOutput) -> Vec<&rootlight_ir::ExtensionEnvelope> {
    result
        .document()
        .extensions
        .iter()
        .filter(|envelope| envelope.namespace == rootlight_ir::RUST_DIRECT_CALL_NAMESPACE)
        .collect()
}

const MARKDOWN: LanguageCase = LanguageCase {
    name: "markdown",
    path: "docs/bindings.md",
    frontend: "tree-sitter-md-0.5.3",
    source: "~~~rust\nfn owner() {}\n~~~\n",
    generated: false,
    body_before: "fn owner() {}",
    body_after: "fn owner() { }",
};

fn markdown_output(source: &str) -> AnalysisOutput {
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

#[test]
fn rust_embedded_examples_keep_local_bindings_and_call_proofs_isolated() {
    let source = "# Examples λ😀\n\n~~~rust\nfn target/*first*/() {} fn owner/*first*/() { let value/*first*/ = 1; value/*read-first*/; target/*call-first*/(); }\n~~~\n\n~~~rust\nfn target/*second*/() {} fn owner/*second*/() { let value/*second*/ = 2; value/*read-second*/; target/*call-second*/(); }\n~~~\n\n~~~rust\nfn owner/*missing*/() { target/*call-missing*/(); value/*read-missing*/; }\n~~~\n\n~~~rust\nfn broken(\n~~~\n";
    let result = markdown_output(source);
    let document = result.document();
    let first = definition(document, source, "value/*first*/", "value");
    let second = definition(document, source, "value/*second*/", "value");
    assert_ne!(first, second);
    reference(
        document,
        source,
        "value/*read-first*/",
        "value",
        Some(first),
    );
    reference(
        document,
        source,
        "value/*read-second*/",
        "value",
        Some(second),
    );
    reference(document, source, "value/*read-missing*/", "value", None);
    let proofs = direct_proofs(&result);
    assert_eq!(
        proofs.len(),
        2,
        "clean examples require separate source proofs"
    );
    let mut owners = BTreeSet::new();
    for suffix in ["first", "second"] {
        let target = definition(document, source, &format!("target/*{suffix}*/"), "target");
        let (start, end) = site(source, &format!("target/*call-{suffix}*/"), "target");
        let envelope = proofs
            .iter()
            .find(|envelope| {
                envelope.evidence.source.as_ref().is_some_and(|source| {
                    source.span().start_byte() == start && source.span().end_byte() == end
                })
            })
            .expect("exact call proof");
        let proof = rootlight_ir::decode_rust_direct_call_envelope(envelope).unwrap();
        assert_eq!(proof.target, target);
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == target)
            .unwrap();
        let Some(rootlight_ir::ContainerRef::Entity(owner)) = entity.container else {
            panic!("written example owns its functions");
        };
        assert!(owners.insert(owner));
        let witness = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.syntax_kind == "markdown.code_block.module"
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: owner }
            })
            .expect("exact written example definition");
        let block = document
            .entities
            .iter()
            .find(|entity| entity.id == owner)
            .unwrap();
        assert_eq!(block.language, "markdown");
        assert_eq!(block.kind, EntityKind::Module);
        assert_eq!(block.canonical_name, "<code-block>");
        assert!(block.flags.contains(&EntityFlag::Synthetic));
        assert_eq!(witness.evidence.source.as_ref(), Some(&witness.source));
        assert_eq!(block.evidence.source.as_ref(), Some(&witness.source));
        assert_eq!(witness.provenance, envelope.provenance);
        assert_eq!(
            witness.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert!(witness.source.span().start_byte() <= proof.definition.start_byte());
        assert!(witness.source.span().end_byte() >= end);
    }
    for call in document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
    {
        assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.detail == "rust-call-dispatch-target-unavailable" && gap.source == call.source
        }));
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn rust_embedded_namespace_and_activation_barriers_stay_within_their_example() {
    let clean = "fn target() {} fn owner() { let before = || target(); let target = target(); target(); before(); }";
    let mut source = format!("~~~rust\n{clean}\n~~~\n");
    for body in [
        "#[cfg(unknown)] fn target() {} fn owner() { target(); }",
        "use opaque::*; fn target() {} fn owner() { target(); }",
        "mod opaque; fn target() {} fn owner() { target(); }",
        "fn target() {} fn owner() { unknown!(); target(); }",
        "fn target() {} fn owner(target: fn()) { target(); }",
        "fn target() {} fn owner() { let target = || (); target(); }",
        "fn target() {} fn owner() { receiver.target(); other::target(); }",
        "fn target() {} fn target() {} fn owner() { target(); }",
        "fn target() {} fn owner() { fn target() {} target(); }",
        "fn target() {} fn owner() { target(); fn target<const N: usize>() {} }",
        "fn target() {} mod nested { fn owner() { target(); } }",
    ] {
        source.push_str(&format!("\n~~~rust\n{body}\n~~~\n"));
    }
    let result = markdown_output(&source);
    let proofs = direct_proofs(&result);
    assert_eq!(
        proofs.len(),
        2,
        "closure creation and whole-let initializer retain their environment"
    );
    let clean_end = u64::try_from(source.find("~~~\n").unwrap()).unwrap();
    for envelope in proofs {
        assert!(envelope.evidence.source.as_ref().unwrap().span().end_byte() < clean_end);
    }
    for call in result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
    {
        assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.detail == "rust-call-dispatch-target-unavailable" && gap.source == call.source
        }));
    }
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn rust_embedded_raw_nfc_and_utf16_proofs_retain_original_physical_evidence() {
    let source = "# Examples λ😀\n\n~~~rust\nfn r#target() {} fn e\u{301}() {} fn owner() { let λ = 1; λ; target(); é(); }\n~~~\n\n~~~rust\nfn r#target() {} fn e\u{301}() {} fn owner() { let λ = 2; λ; target(); é(); }\n~~~\n";
    let mut identities = None;
    for encoding in [None, Some(true), Some(false)] {
        let mut bytes = match encoding {
            None => source.as_bytes().to_vec(),
            Some(true) => vec![0xff, 0xfe],
            Some(false) => vec![0xfe, 0xff],
        };
        if let Some(little) = encoding {
            for unit in source.encode_utf16() {
                bytes.extend(if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        let provider = Arc::new(provider());
        let fixture = Fixture::new(MARKDOWN, &bytes);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(MARKDOWN),
            EncodingId::new(match encoding {
                None => "utf-8",
                Some(true) => "utf-16le",
                Some(false) => "utf-16be",
            })
            .unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, MARKDOWN),
            &request,
            &ExtensionSupport::default(),
        );
        validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        let physical = |offset: usize| {
            u64::try_from(if encoding.is_none() {
                offset
            } else {
                2 + source[..offset].encode_utf16().count() * 2
            })
            .unwrap()
        };
        let proofs = direct_proofs(&result);
        assert_eq!(proofs.len(), 4);
        for (written, callee, canonical) in
            [("r#target", "target", "target"), ("e\u{301}", "é", "é")]
        {
            let definitions: Vec<_> = source
                .match_indices(written)
                .map(|(start, _)| start)
                .collect();
            for (ordinal, (start, _)) in source.match_indices(&format!("{callee}();")).enumerate() {
                let envelope = proofs
                    .iter()
                    .find(|envelope| {
                        envelope
                            .evidence
                            .source
                            .as_ref()
                            .unwrap()
                            .span()
                            .start_byte()
                            == physical(start)
                    })
                    .unwrap();
                let proof = rootlight_ir::decode_rust_direct_call_envelope(envelope).unwrap();
                assert_eq!(
                    proof.definition.start_byte(),
                    physical(definitions[ordinal])
                );
                assert_eq!(
                    proof.definition.end_byte(),
                    physical(definitions[ordinal] + written.len())
                );
                assert_eq!(proof.canonical_name, content_hash(canonical.as_bytes()));
                let site = envelope.evidence.source.as_ref().unwrap();
                assert_eq!(site.span().end_byte(), physical(start + callee.len()));
                assert_eq!(site.content_hash(), content_hash(&bytes));
                let authored = if let Some(little) = encoding {
                    callee
                        .encode_utf16()
                        .flat_map(|unit| {
                            if little {
                                unit.to_le_bytes()
                            } else {
                                unit.to_be_bytes()
                            }
                        })
                        .collect::<Vec<_>>()
                } else {
                    callee.as_bytes().to_vec()
                };
                assert_eq!(
                    &bytes[usize::try_from(site.span().start_byte()).unwrap()
                        ..usize::try_from(site.span().end_byte()).unwrap()],
                    authored
                );
                let call = result
                    .document()
                    .occurrences
                    .iter()
                    .find(|call| call.role == OccurrenceRole::CallSite && call.source == *site)
                    .unwrap();
                assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
                assert_eq!(call.syntactic_text_hash, content_hash(callee.as_bytes()));
            }
        }
        let current: BTreeSet<_> = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.language == "rust")
            .map(|entity| entity.id)
            .collect();
        assert_eq!(
            current.len(),
            8,
            "each example owns three functions and one local"
        );
        match &identities {
            Some(expected) => assert_eq!(&current, expected),
            None => identities = Some(current),
        }
        let local_reads: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Reference
                    && occurrence.syntactic_text_hash == content_hash("λ".as_bytes())
            })
            .collect();
        assert_eq!(local_reads.len(), 2);
        for read in local_reads {
            assert!(matches!(read.target, OccurrenceTarget::Resolved { .. }));
        }
    }
}

#[test]
fn rust_embedded_required_only_replay_preserves_local_and_closure_identity() {
    let source = "~~~rust\nfn target() {} fn owner(input: i32) { let value = input; let value = value; let closure = |value: i32| { value }; closure(value); target(); }\n~~~\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let full_limits = limits();
    let full_request = request(&fixture.snapshot, &fixture.source, MARKDOWN, &full_limits);
    let full_artifact = analyzer
        .prepare_artifact(&full_request, &deadline())
        .unwrap();
    let required = full_artifact
        .required_syntax_fact_count(&deadline())
        .unwrap();
    assert!(full_artifact.syntax_fact_count() > required);
    let full = analyze(&analyzer, &full_request, &ExtensionSupport::default());
    assert_eq!(
        direct_proofs(&full).len(),
        1,
        "complete native context: {:?}",
        rootlight_adapter_sdk::execute_parse(
            provider.as_ref(),
            &full_request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline()
        )
        .unwrap()
        .facts()
    );
    let bounded_limits = limits_with_syntax_records(required);
    let bounded_request = request(
        &fixture.snapshot,
        &fixture.source,
        MARKDOWN,
        &bounded_limits,
    );
    let bounded_artifact = analyzer
        .prepare_artifact(&bounded_request, &deadline())
        .unwrap();
    assert!(!bounded_artifact.syntax_partition_complete());
    assert_eq!(
        bounded_artifact
            .required_syntax_fact_count(&deadline())
            .unwrap(),
        required
    );
    let bounded = analyze(&analyzer, &bounded_request, &ExtensionSupport::default());
    let identities = |output: &AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.language == "rust")
            .map(|entity| (entity.id, entity.container))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        identities(&bounded),
        identities(&full),
        "optional extraction cannot change mandatory local/closure identity or ownership"
    );
    let containment = |output: &AnalysisOutput| {
        output
            .document()
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::Contains)
            .map(|relation| (relation.subject, relation.object))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(containment(&bounded), containment(&full));
    assert!(direct_proofs(&bounded).is_empty());
    assert!(
        bounded
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-extraction-limit")
    );
    let replay = analyzer
        .analyze_prepared_artifact(
            &bounded_request,
            &bounded_artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(replay.document(), bounded.document());
    assert_eq!(replay.report(), bounded.report());
    validate_ir_document(
        replay.document(),
        bounded_limits.ir(),
        &ExtensionSupport::default(),
    )
    .unwrap();
}

#[test]
fn rust_direct_function_calls_carry_exact_source_proof_without_resolving_natively() {
    let source = "fn r#target() {}\nfn owner() { target(); let target = target(); target(); }\n";
    let result = output(source);
    let proofs = direct_proofs(&result);
    assert_eq!(
        proofs.len(),
        2,
        "both pre-binding identifier calls require explicit source proof"
    );
    let declaration = result
        .document()
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.source.span().start_byte() == 3
        })
        .unwrap();
    let OccurrenceTarget::Resolved { symbol } = declaration.target else {
        panic!("function definition is materialized");
    };
    for envelope in proofs {
        let proof = rootlight_ir::decode_rust_direct_call_envelope(envelope).unwrap();
        assert_eq!(proof.target, symbol);
        assert_eq!(proof.definition, declaration.source.span());
        assert_eq!(proof.canonical_name, content_hash(b"target"));
        assert_eq!(
            envelope.evidence.derivation,
            vec![
                rootlight_ir::FactRef::File(declaration.file),
                rootlight_ir::FactRef::Entity(symbol)
            ]
        );
        let site = envelope.evidence.source.as_ref().unwrap();
        assert_eq!(
            &source.as_bytes()[usize::try_from(site.span().start_byte()).unwrap()
                ..usize::try_from(site.span().end_byte()).unwrap()],
            b"target"
        );
        let call = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.source == *site && occurrence.role == OccurrenceRole::CallSite
            })
            .unwrap();
        assert_eq!(call.syntax_kind, "rust.direct_call.call");
        assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
    }
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn rust_direct_function_proof_rejects_receiver_shadow_namespace_and_missing_context() {
    for source in [
        "fn target() {} fn owner(target: fn()) { target(); }",
        "fn target() {} fn owner() { let target = || (); target(); }",
        "fn target() {} fn owner() { receiver.target(); other::target(); }",
        "fn target() {} fn target() {} fn owner() { target(); }",
        "#[cfg(unknown)] fn target() {} fn owner() { target(); }",
        "fn target() {} fn owner() { unknown!(); target(); }",
        "use opaque::*; fn target() {} fn owner() { target(); }",
        "mod opaque; fn target() {} fn owner() { target(); }",
        "fn target() {} fn owner() { fn target() {} target(); }",
        "fn target() {} mod nested { fn owner() { target(); } }",
        "fn target() {} fn owner() { static target: fn() = || (); target(); }",
        "fn target() {} struct Owner; impl Owner { fn owner() { target(); } }",
    ] {
        let result = output(source);
        assert!(
            direct_proofs(&result).is_empty(),
            "unproven context must not emit a claim: {source}"
        );
        for call in result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        {
            assert!(
                matches!(call.target, OccurrenceTarget::Unresolved { .. }),
                "{source}"
            );
            assert!(
                result
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "rust-call-dispatch-target-unavailable"
                        && gap.source.span() == call.source.span()),
                "missing exact retained call gap: {source}"
            );
        }
    }
}

#[test]
fn rust_direct_call_proofs_preserve_raw_nfc_and_utf16_physical_sources() {
    let source = "fn r#target() {} fn e\u{301}() {} fn owner() { target(); é(); }";
    let mut ids = None;
    for encoding in [None, Some(true), Some(false)] {
        let mut bytes = match encoding {
            None => source.as_bytes().to_vec(),
            Some(true) => vec![0xff, 0xfe],
            Some(false) => vec![0xfe, 0xff],
        };
        if let Some(little) = encoding {
            for unit in source.encode_utf16() {
                bytes.extend(if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        let provider = Arc::new(provider());
        let fixture = Fixture::new(RUST, &bytes);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(RUST),
            EncodingId::new(match encoding {
                None => "utf-8",
                Some(true) => "utf-16le",
                Some(false) => "utf-16be",
            })
            .unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, RUST),
            &request,
            &ExtensionSupport::default(),
        );
        validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        let physical = |offset: usize| {
            u64::try_from(if encoding.is_none() {
                offset
            } else {
                2 + source[..offset].encode_utf16().count() * 2
            })
            .unwrap()
        };
        let proofs = direct_proofs(&result);
        assert_eq!(proofs.len(), 2);
        let mut current = Vec::new();
        for (declaration, call, canonical) in [
            ("r#target", "target();", "target"),
            ("e\u{301}", "é();", "é"),
        ] {
            let start = source.find(call).unwrap();
            let name = call.strip_suffix("();").unwrap();
            let envelope = proofs
                .iter()
                .find(|envelope| {
                    envelope
                        .evidence
                        .source
                        .as_ref()
                        .unwrap()
                        .span()
                        .start_byte()
                        == physical(start)
                })
                .unwrap();
            let witness = rootlight_ir::decode_rust_direct_call_envelope(envelope).unwrap();
            let definition_start = source.find(declaration).unwrap();
            assert_eq!(witness.definition.start_byte(), physical(definition_start));
            assert_eq!(
                witness.definition.end_byte(),
                physical(definition_start + declaration.len())
            );
            assert_eq!(witness.canonical_name, content_hash(canonical.as_bytes()));
            let site = envelope.evidence.source.as_ref().unwrap();
            assert_eq!(site.content_hash(), content_hash(&bytes));
            assert_eq!(site.span().end_byte(), physical(start + name.len()));
            let authored = if let Some(little) = encoding {
                name.encode_utf16()
                    .flat_map(|unit| {
                        if little {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect::<Vec<_>>()
            } else {
                name.as_bytes().to_vec()
            };
            assert_eq!(
                &bytes[usize::try_from(site.span().start_byte()).unwrap()
                    ..usize::try_from(site.span().end_byte()).unwrap()],
                authored
            );
            let occurrence = result
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.role == OccurrenceRole::CallSite && occurrence.source == *site
                })
                .unwrap();
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(name.as_bytes())
            );
            current.push(witness.target);
        }
        match &ids {
            Some(expected) => assert_eq!(&current, expected),
            None => ids = Some(current),
        }
    }
}

#[test]
fn rust_const_generic_callers_do_not_prove_an_outer_function_target() {
    for (definition, parameter, name) in [
        ("helper", "helper", "helper"),
        ("r#helper", "r#helper", "helper"),
        ("r#é", "r#e\u{301}", "é"),
    ] {
        let source = format!(
            "// \u{3bb}\u{1f600}\nfn {definition}() {{}} fn owner<const {parameter}: usize>() {{ {name}(); }} fn sibling() {{ {name}(); }}"
        );
        for encoding in [None, Some(true), Some(false)] {
            let bytes = match encoding {
                None => source.as_bytes().to_vec(),
                Some(little) => std::iter::once(0xfeff_u16)
                    .chain(source.encode_utf16())
                    .flat_map(|unit| {
                        if little {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect(),
            };
            let parser = Arc::new(provider());
            let fixture = Fixture::new(RUST, &bytes);
            let budget = limits();
            let request = AnalysisRequest::new_with_parse_context(
                GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
                language(RUST),
                EncodingId::new(match encoding {
                    None => "utf-8",
                    Some(true) => "utf-16le",
                    Some(false) => "utf-16be",
                })
                .unwrap(),
                Vec::new(),
                AnalysisTier::TierD,
                build_context(),
                &budget,
            )
            .unwrap()
            .with_generated_status(false);
            let result = analyze(
                &analyzer(&parser, RUST),
                &request,
                &ExtensionSupport::default(),
            );
            let document = result.document();
            validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
            let physical = |position: usize| {
                u64::try_from(match encoding {
                    None => position,
                    Some(_) => 2 + source[..position].encode_utf16().count() * 2,
                })
                .unwrap()
            };
            let authored_call = format!("{name}();");
            let shadow_start = source.find(&authored_call).unwrap();
            let shadow_calls = document
                .occurrences
                .iter()
                .filter(|call| {
                    call.role == OccurrenceRole::CallSite
                        && call.source.span().start_byte() == physical(shadow_start)
                        && call.source.span().end_byte() == physical(shadow_start + name.len())
                })
                .collect::<Vec<_>>();
            assert_eq!(shadow_calls.len(), 1, "{parameter}: {encoding:?}");
            let shadow = shadow_calls[0];
            assert!(matches!(shadow.target, OccurrenceTarget::Unresolved { .. }));
            assert_eq!(shadow.source.content_hash(), content_hash(&bytes));
            assert_eq!(shadow.syntactic_text_hash, content_hash(name.as_bytes()));
            let proofs = direct_proofs(&result);
            assert!(
                !proofs
                    .iter()
                    .any(|proof| proof.evidence.source.as_ref() == Some(&shadow.source)),
                "a const generic shadows the outer function: {parameter}: {encoding:?}"
            );
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.source == shadow.source && gap.detail == "rust-call-dispatch-target-unavailable"
            }));
            let sibling_start = source.rfind(&authored_call).unwrap();
            assert_eq!(proofs.len(), 1, "the sibling retains its written proof");
            assert_eq!(
                proofs[0].evidence.source.as_ref().unwrap().span(),
                SourceSpan::new(
                    fixture.snapshot.file(),
                    physical(sibling_start),
                    physical(sibling_start + name.len()),
                )
                .unwrap()
            );
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Calls)
            );
        }
    }
}

#[test]
fn rust_generic_calls_retain_exact_unresolved_callees_in_three_encodings() {
    let source = concat!(
        "// λ😀\nfn r#e\u{301}<T>(value: T) {}\n",
        "mod api { pub fn r#e\u{301}<T>(value: T) {} }\n",
        "fn owner(receiver: &Receiver) {\n",
        "r#é::<u8>(1); crate::api::r#é::<u8>(1); receiver.item::<u8>(1);\n}\n",
    );
    for encoding in [None, Some(true), Some(false)] {
        let bytes = match encoding {
            None => source.as_bytes().to_vec(),
            Some(little) => std::iter::once(0xfeff_u16)
                .chain(source.encode_utf16())
                .flat_map(|unit| {
                    if little {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect(),
        };
        let parser = Arc::new(provider());
        let fixture = Fixture::new(RUST, &bytes);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(RUST),
            EncodingId::new(match encoding {
                None => "utf-8",
                Some(true) => "utf-16le",
                Some(false) => "utf-16be",
            })
            .unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&parser, RUST),
            &request,
            &ExtensionSupport::default(),
        );
        let document = result.document();
        validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
        let calls = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::CallSite
                    && site.syntax_kind == "rust.generic_call.call"
            })
            .collect::<Vec<_>>();
        assert_eq!(
            calls.len(),
            3,
            "every generic callee must be retained: {encoding:?}"
        );
        let physical = |position: usize| {
            u64::try_from(match encoding {
                None => position,
                Some(_) => 2 + source[..position].encode_utf16().count() * 2,
            })
            .unwrap()
        };
        for callee in ["r#é::<u8>", "crate::api::r#é::<u8>", "receiver.item::<u8>"] {
            let start = source.find(callee).unwrap();
            let call = calls
                .iter()
                .find(|call| call.source.span().start_byte() == physical(start))
                .unwrap();
            assert_eq!(
                call.source.span().end_byte(),
                physical(start + callee.len())
            );
            assert_eq!(call.source.content_hash(), content_hash(&bytes));
            assert_eq!(call.syntactic_text_hash, content_hash(callee.as_bytes()));
            assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
            let authored = match encoding {
                None => callee.as_bytes().to_vec(),
                Some(little) => callee
                    .encode_utf16()
                    .flat_map(|unit| {
                        if little {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect(),
            };
            assert_eq!(
                &bytes[usize::try_from(call.source.span().start_byte()).unwrap()
                    ..usize::try_from(call.source.span().end_byte()).unwrap()],
                authored
            );
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == call.source
                        && gap.detail == "rust-generic-instantiation-and-typecheck-unverified")
            );
        }
        assert!(direct_proofs(&result).is_empty());
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
    }
}
