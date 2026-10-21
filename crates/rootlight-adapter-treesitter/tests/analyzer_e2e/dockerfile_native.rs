//! Qualify complete original Docker source and conservative written-stage proof.
//! A clean native tree does not establish image, argument, shell or build execution
//! semantics; exact alias sites and COPY ownership are tested independently.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/dockerfile/original.Dockerfile");
const DOCKERFILE: LanguageCase = LanguageCase {
    name: "dockerfile",
    path: "Dockerfile",
    frontend: "dockerfile-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let native = Arc::new(provider());
    let fixture = Fixture::new(DOCKERFILE, bytes);
    let bounds = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(DOCKERFILE),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &bounds,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&native, DOCKERFILE),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), bounds.ir(), &ExtensionSupport::default()).unwrap();
    for site in &result.document().occurrences {
        assert_eq!(site.source.repository(), fixture.source.repository());
        assert_eq!(site.source.generation(), fixture.source.generation());
        assert_eq!(site.source.span().file(), fixture.source.span().file());
        assert_eq!(site.source.content_hash(), content_hash(bytes));
        assert!(site.source.span().end_byte() <= u64::try_from(bytes.len()).unwrap());
    }
    result
}

fn definition(result: &AnalysisOutput, name: &str, start: u64, end: u64) -> SymbolId {
    let sites: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "exact written stage {name}@{start}..{end}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("written stage definition resolves")
    };
    let entity = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::BuildTarget);
    assert_eq!(entity.language, "dockerfile");
    assert_eq!(entity.canonical_name, name);
    assert_eq!(
        entity.container,
        Some(rootlight_ir::ContainerRef::File(
            sites[0].source.span().file()
        ))
    );
    symbol
}

fn dependency(result: &AnalysisOutput, owner: SymbolId, target: SymbolId, start: u64, end: u64) {
    let sites: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].syntax_kind, "dockerfile.copy_from.reference");
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    assert_eq!(sites[0].enclosing, Some(owner));
    let relations: Vec<_> = result
        .document()
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::DependsOn
                && relation.subject == RelationEndpoint::Entity(owner)
                && relation.object == RelationEndpoint::Entity(target)
                && relation.evidence.source.as_ref() == Some(&sites[0].source)
        })
        .collect();
    assert_eq!(relations.len(), 1);
    assert!(
        relations[0]
            .evidence
            .derivation
            .contains(&rootlight_ir::FactRef::Fact(sites[0].id))
    );
    assert!(!result.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::DependsOn
            && relation.subject == RelationEndpoint::Entity(target)
            && relation.object == RelationEndpoint::Entity(owner)
    }));
}

fn no_dependency(result: &AnalysisOutput) {
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::DependsOn)
    );
    for site in &result.document().occurrences {
        if site.syntax_kind == "dockerfile.copy_from.reference" {
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        }
    }
}

#[test]
fn whole_original_certifies_only_written_aliases_and_forward_copy_dependency() {
    assert_eq!(ORIGINAL.len(), 1484);
    assert_eq!(
        ORIGINAL.windows(2).filter(|part| *part == b"\r\n").count(),
        41
    );
    let result = output(ORIGINAL, EncodingId::utf8());
    let frontend = definition(&result, "frontend", 28, 36);
    let backend = definition(&result, "backend", 425, 432);
    dependency(&result, backend, frontend, 500, 508);
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::BuildTarget)
            .count(),
        2
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == domain)
        );
    }
}

#[test]
fn ordinary_stage_names_and_lf_crlf_layout_keep_exact_dependency_source() {
    for bytes in [
        b"FROM scratch AS compile-one\nRUN true\nFROM scratch AS final-two\nCOPY --from=compile-one /a /b\n".as_slice(),
        b"FROM scratch AS compile-one\r\nRUN true\r\nFROM scratch AS final-two\r\nCOPY --from=compile-one /a /b\r\n".as_slice(),
    ] {
        let result = output(bytes, EncodingId::utf8());
        let text = std::str::from_utf8(bytes).unwrap();
        let a = u64::try_from(text.find("compile-one").unwrap()).unwrap();
        let b = u64::try_from(text.find("final-two").unwrap()).unwrap();
        let at = u64::try_from(text.rfind("compile-one").unwrap()).unwrap();
        let source = definition(&result, "compile-one", a, a + 11);
        let owner = definition(&result, "final-two", b, b + 9);
        dependency(&result, owner, source, at, at + 11);
    }
}

#[test]
fn duplicate_forward_self_and_unnamed_stage_owners_never_gain_generic_targets() {
    for bytes in [
        b"FROM scratch AS earlier\nFROM scratch AS earlier\nFROM scratch AS final\nCOPY --from=earlier /a /b\n".as_slice(),
        b"FROM scratch AS earlier\nCOPY --from=later /a /b\nFROM scratch AS later\n".as_slice(),
        b"FROM scratch AS earlier\nCOPY --from=earlier /a /b\n".as_slice(),
        b"FROM scratch AS earlier\nFROM scratch\nCOPY --from=earlier /a /b\n".as_slice(),
        b"FROM scratch AS earlier\nFROM scratch AS EARLIER\nFROM scratch AS final\nCOPY --from=earlier /a /b\n".as_slice(),
    ] {
        no_dependency(&output(bytes, EncodingId::utf8()));
    }
}

#[test]
fn quoted_dynamic_numeric_external_and_repeated_flags_remain_unproved() {
    for flag in [
        "'earlier'",
        "\"earlier\"",
        "${stage}",
        "0",
        "external-image",
        "earlier --from=earlier",
    ] {
        let source =
            format!("FROM scratch AS earlier\nFROM scratch AS final\nCOPY --from={flag} /a /b\n");
        no_dependency(&output(source.as_bytes(), EncodingId::utf8()));
    }
}

#[test]
fn comments_shell_strings_and_image_names_are_not_stage_definitions() {
    let source = b"# FROM scratch AS fake\nFROM unrelated-image AS actual\nRUN echo 'FROM scratch AS decoy COPY --from=actual /a /b'\n";
    let result = output(source, EncodingId::utf8());
    let names: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::BuildTarget)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(names, ["actual"]);
    no_dependency(&result);
}

#[test]
fn recovered_native_roots_withhold_stage_authority_and_keep_explicit_gaps() {
    let source =
        b"@invalid!\nFROM scratch AS earlier\nFROM scratch AS final\nCOPY --from=earlier /a /b\n";
    let result = output(source, EncodingId::utf8());
    no_dependency(&result);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::BuildTarget)
    );
    assert!(!result.document().skipped_regions.is_empty());
}

#[test]
fn utf16_original_keeps_physical_sites_and_original_content_identity() {
    for little in [true, false] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in std::str::from_utf8(ORIGINAL).unwrap().encode_utf16() {
            bytes.extend(if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let result = output(
            &bytes,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        let frontend = definition(&result, "frontend", 2 + 28 * 2, 2 + 36 * 2);
        let backend = definition(&result, "backend", 2 + 425 * 2, 2 + 432 * 2);
        dependency(&result, backend, frontend, 2 + 500 * 2, 2 + 508 * 2);
    }
}

#[test]
fn cancellation_and_changed_source_reject_retained_stage_proof() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, DOCKERFILE);
    let fixture = Fixture::new(DOCKERFILE, ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, DOCKERFILE, &bounds);
    let artifact = frontend.prepare_artifact(&input, &deadline()).unwrap();
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        frontend.analyze_from_artifact(
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
    let changed = fixture.rewrite(b"FROM scratch AS different\n");
    let input = request(&changed.snapshot, &changed.source, DOCKERFILE, &bounds);
    assert!(
        matches!(frontend.analyze_from_artifact(&input, &artifact, ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
}

#[test]
fn raw_unnamed_from_boundary_cannot_be_crossed_by_replayed_stage_extent() {
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};

    let bytes = b"FROM scratch AS earlier\nFROM scratch AS final\nCOPY --from=earlier /a /b\nFROM scratch\nCOPY --from=earlier /c /d\n";
    let text = std::str::from_utf8(bytes).unwrap();
    let native = provider();
    let fixture = Fixture::new(DOCKERFILE, bytes);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, DOCKERFILE, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
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
            language(DOCKERFILE),
            DOCKERFILE.frontend,
            content_hash(BINARY_SEED),
        )
        .unwrap();
        let result = execute_analysis(
            &frontend,
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        validate_ir_document(result.document(), bounds.ir(), &ExtensionSupport::default()).unwrap();
        result
    };
    let baseline = replay(parsed.facts().to_vec());
    let earlier_at = u64::try_from(text.find("earlier").unwrap()).unwrap();
    let final_at = u64::try_from(text.find("final").unwrap()).unwrap();
    let first_copy_at = u64::try_from(text.find("--from=earlier").unwrap() + 7).unwrap();
    let late_copy_at = u64::try_from(text.rfind("earlier").unwrap()).unwrap();
    let unnamed_at = u64::try_from(text.find("FROM scratch\n").unwrap()).unwrap();
    let earlier = definition(&baseline, "earlier", earlier_at, earlier_at + 7);
    let owner = definition(&baseline, "final", final_at, final_at + 5);
    dependency(&baseline, owner, earlier, first_copy_at, first_copy_at + 7);
    let late_site = |result: &AnalysisOutput| {
        result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "dockerfile.copy_from.reference"
                    && site.source.span().start_byte() == late_copy_at
                    && site.source.span().end_byte() == late_copy_at + 7
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    let baseline_late = late_site(&baseline);
    assert_eq!(baseline_late.len(), 1);
    assert!(matches!(
        baseline_late[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_ne!(baseline_late[0].enclosing, Some(owner));

    let headers = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "dockerfile.stage_header.signature")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        headers.len(),
        3,
        "the unnamed FROM must remain a raw boundary"
    );
    assert!(
        headers
            .iter()
            .any(|fact| fact.span().start_byte() == unnamed_at)
    );
    let definitions = parsed
        .facts()
        .iter()
        .filter(|fact| {
            fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "dockerfile.stage_alias.definition"
                && fact.span().start_byte() == final_at
                && fact.span().end_byte() == final_at + 5
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1);
    let current_local = definitions[0].parent().unwrap();
    let declarations = parsed
        .facts()
        .iter()
        .filter(|fact| fact.local_id() == current_local)
        .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 1);
    let current = declarations[0];
    assert_eq!(current.kind(), SyntaxFactKind::Declaration);
    assert_eq!(
        current.syntax_kind().as_str(),
        "dockerfile.stage.declaration"
    );
    assert_eq!(current.span().end_byte(), unnamed_at);
    let late_references = parsed
        .facts()
        .iter()
        .filter(|fact| {
            fact.syntax_kind().as_str() == "dockerfile.copy_from.reference"
                && fact.span().start_byte() == late_copy_at
                && fact.span().end_byte() == late_copy_at + 7
        })
        .collect::<Vec<_>>();
    assert_eq!(late_references.len(), 1);
    let late_reference = late_references[0];
    assert_ne!(late_reference.parent(), Some(current_local));
    let late_contexts = parsed
        .facts()
        .iter()
        .filter(|fact| {
            fact.syntax_kind().as_str() == "dockerfile.copy_from_context.signature"
                && fact.span().start_byte() <= late_copy_at
                && fact.span().end_byte() >= late_copy_at + 7
        })
        .collect::<Vec<_>>();
    assert_eq!(late_contexts.len(), 1);
    let late_context = late_contexts[0];
    let mut facts = parsed.facts().to_vec();
    for fact in &mut facts {
        let span = if fact.local_id() == current_local {
            SourceSpan::new(
                fact.span().file(),
                fact.span().start_byte(),
                u64::try_from(bytes.len()).unwrap(),
            )
            .unwrap()
        } else {
            fact.span()
        };
        let parent = if fact.local_id() == late_reference.local_id()
            || fact.local_id() == late_context.local_id()
        {
            Some(current_local)
        } else {
            fact.parent()
        };
        let depth = if fact.local_id() == late_reference.local_id()
            || fact.local_id() == late_context.local_id()
        {
            current.depth().checked_add(1).unwrap()
        } else {
            fact.depth()
        };
        *fact = SyntaxFact::new(
            fact.local_id(),
            parent,
            fact.kind(),
            span,
            depth,
            fact.syntax_kind().clone(),
        );
    }
    assert_eq!(facts.len(), parsed.facts().len());
    let retained_headers = facts
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "dockerfile.stage_header.signature")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        retained_headers, headers,
        "all three physical FROM witnesses stay exact"
    );
    let changed = replay(facts);
    let changed_late = late_site(&changed);
    assert_eq!(changed_late.len(), 1);
    let site = &changed_late[0];
    assert_eq!(site.source, baseline_late[0].source);
    assert_eq!(site.provenance, baseline_late[0].provenance);
    assert_eq!(site.source.content_hash(), content_hash(bytes));
    assert_eq!(site.repository, fixture.source.repository());
    assert_eq!(site.generation, fixture.source.generation());
    assert!(
        matches!(site.target, OccurrenceTarget::Unresolved { .. }),
        "an enlarged stage extent must not cross the retained unnamed FROM"
    );
    assert!(!changed.document().relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::DependsOn
            && edge.evidence.source.as_ref() == Some(&site.source)
    }));
    assert!(changed.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Relations
            && gap.source == site.source
            && gap.detail == "dockerfile-copy-source-is-not-a-unique-prior-written-stage"
    }));
}

const ENV_ORIGINAL: &[u8] = include_bytes!("../fixtures/dockerfile_env/original.Dockerfile");

fn env_definition(result: &AnalysisOutput, name: &str, start: u64, end: u64) -> SymbolId {
    let sites: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "exact written ENV {name}@{start}..{end}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("written ENV definition resolves")
    };
    let entities: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.id == symbol)
        .collect();
    assert_eq!(entities.len(), 1);
    let entity = entities[0];
    assert_eq!(entity.kind, EntityKind::Variable);
    assert_eq!(entity.language, "dockerfile");
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.provenance, sites[0].provenance);
    let source = entity.evidence.source.as_ref().unwrap();
    assert_eq!(source.repository(), sites[0].source.repository());
    assert_eq!(source.generation(), sites[0].source.generation());
    assert_eq!(source.content_hash(), sites[0].source.content_hash());
    assert!(source.span().start_byte() <= start && source.span().end_byte() >= end);
    symbol
}

fn env_no_nested_assignment(result: &AnalysisOutput, left: SymbolId, right: SymbolId) {
    assert_ne!(left, right);
    for (parent, child) in [(left, right), (right, left)] {
        assert!(!result.document().relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(parent)
                && edge.object == RelationEndpoint::Entity(child)
        }));
    }
}

#[test]
fn complete_env_original_has_two_written_variables_without_nested_assignment_or_value_entities() {
    assert_eq!(ENV_ORIGINAL.len(), 475);
    assert_eq!(&ENV_ORIGINAL[206..210], b"HOST");
    assert_eq!(&ENV_ORIGINAL[224..228], b"PORT");
    assert_eq!(
        ENV_ORIGINAL
            .windows(2)
            .filter(|bytes| *bytes == b"\r\n")
            .count(),
        22
    );
    let result = output(ENV_ORIGINAL, EncodingId::utf8());
    let host = env_definition(&result, "HOST", 206, 210);
    let port = env_definition(&result, "PORT", 224, 228);
    env_no_nested_assignment(&result, host, port);
    for id in [host, port] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == id)
            .unwrap();
        assert_eq!(
            entity.container,
            Some(rootlight_ir::ContainerRef::File(
                result.document().files[0].id
            ))
        );
    }
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable)
            .count(),
        2
    );
    for (start, end) in [(229, 233), (450, 454)] {
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end)
        );
    }
    assert!(!result.document().relations.iter().any(|edge| matches!(
        edge.predicate,
        RelationPredicate::Calls | RelationPredicate::Reads | RelationPredicate::Writes
    )));
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == domain)
        );
    }
}

#[test]
fn ordinary_env_pairs_keep_independent_written_sites_and_ignore_other_directive_names() {
    let bytes = b"FROM scratch\n# ENV comment=never\nENV _alpha=literal Beta2=\"two words\" lower=\nENV _alpha=again\nARG ignored=other\nRUN echo 'ENV shell=fake'\nENV legacy old\n";
    let result = output(bytes, EncodingId::utf8());
    let text = std::str::from_utf8(bytes).unwrap();
    let mut ids = Vec::new();
    for (name, at) in [
        ("_alpha", text.find("_alpha").unwrap()),
        ("Beta2", text.find("Beta2").unwrap()),
        ("lower", text.find("lower").unwrap()),
        ("_alpha", text.rfind("_alpha").unwrap()),
    ] {
        ids.push(env_definition(
            &result,
            name,
            at as u64,
            (at + name.len()) as u64,
        ));
    }
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        4
    );
    env_no_nested_assignment(&result, ids[0], ids[1]);
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable)
            .count(),
        4
    );
    for name in ["comment", "ignored", "shell", "legacy"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
}

#[test]
fn env_original_bom_and_utf16_preserve_exact_physical_name_sites_and_content_identity() {
    let mut bom = b"\xef\xbb\xbf".to_vec();
    bom.extend_from_slice(ENV_ORIGINAL);
    let result = output(&bom, EncodingId::utf8());
    env_no_nested_assignment(
        &result,
        env_definition(&result, "HOST", 209, 213),
        env_definition(&result, "PORT", 227, 231),
    );
    for little in [true, false] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in std::str::from_utf8(ENV_ORIGINAL).unwrap().encode_utf16() {
            bytes.extend(if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let result = output(
            &bytes,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        env_no_nested_assignment(
            &result,
            env_definition(&result, "HOST", 414, 422),
            env_definition(&result, "PORT", 450, 458),
        );
    }
}

#[test]
fn env_current_source_consumer_rejects_missing_duplicate_repositioned_and_reparented_witnesses() {
    use rootlight_adapter_sdk::{
        SyntaxFact, SyntaxFactKind, SyntaxKindLabel, testkit::MockParseProvider,
    };
    let bytes = b"FROM scratch\nENV first=VALUE\nENV second=other\n";
    let fixture = Fixture::new(DOCKERFILE, bytes);
    let native = provider();
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, DOCKERFILE, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts: Vec<SyntaxFact>| -> Result<AnalysisOutput, AdapterError> {
        let parser = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(parser),
            producer_identity(),
            language(DOCKERFILE),
            DOCKERFILE.frontend,
            content_hash(BINARY_SEED),
        )
        .unwrap();
        let result = execute_analysis(
            &frontend,
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )?;
        validate_ir_document(result.document(), bounds.ir(), &ExtensionSupport::default()).unwrap();
        Ok(result)
    };
    let text = std::str::from_utf8(bytes).unwrap();
    let first = text.find("first").unwrap() as u64;
    let second = text.find("second").unwrap() as u64;
    let baseline = replay(parsed.facts().to_vec()).unwrap();
    env_no_nested_assignment(
        &baseline,
        env_definition(&baseline, "first", first, first + 5),
        env_definition(&baseline, "second", second, second + 6),
    );
    let definition = |at| {
        parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.syntax_kind().as_str() == "dockerfile.env_name.definition"
                    && fact.span().start_byte() == at
            })
            .unwrap()
    };
    let first_def = definition(first);
    let second_def = definition(second);
    let owner = |name: &SyntaxFact| {
        parsed
            .facts()
            .iter()
            .find(|fact| Some(fact.local_id()) == name.parent())
            .unwrap()
    };
    let first_decl = owner(first_def);
    let second_decl = owner(second_def);
    let marker = parsed
        .facts()
        .iter()
        .find(|fact| {
            fact.parent() == Some(first_decl.local_id())
                && fact.syntax_kind().as_str() == "dockerfile.env_pair_context.signature"
        })
        .unwrap();
    let context = parsed
        .facts()
        .iter()
        .find(|fact| {
            fact.syntax_kind().as_str() == "dockerfile.env_instruction.signature"
                && fact.span().start_byte() < first
                && fact.span().end_byte() >= first + 5
        })
        .unwrap();
    let root = parsed
        .facts()
        .iter()
        .find(|fact| fact.kind() == SyntaxFactKind::Root)
        .unwrap();
    let new_id = parsed
        .facts()
        .iter()
        .map(SyntaxFact::local_id)
        .max()
        .unwrap()
        + 1;
    for mode in 0..7 {
        let mut facts = parsed.facts().to_vec();
        match mode {
            0 => facts.retain(|fact| fact.local_id() != marker.local_id()),
            1 => facts.push(SyntaxFact::new(
                new_id,
                marker.parent(),
                marker.kind(),
                marker.span(),
                marker.depth(),
                marker.syntax_kind().clone(),
            )),
            2 => facts.push(SyntaxFact::new(
                new_id,
                marker.parent(),
                SyntaxFactKind::Comment,
                marker.span(),
                marker.depth(),
                SyntaxKindLabel::new("dockerfile.comment.comment").unwrap(),
            )),
            3 => {
                let fact = facts
                    .iter_mut()
                    .find(|fact| fact.local_id() == marker.local_id())
                    .unwrap();
                *fact = SyntaxFact::new(
                    fact.local_id(),
                    Some(root.local_id()),
                    fact.kind(),
                    fact.span(),
                    root.depth() + 1,
                    fact.syntax_kind().clone(),
                );
            }
            4 => {
                let fact = facts
                    .iter_mut()
                    .find(|fact| fact.local_id() == first_def.local_id())
                    .unwrap();
                let at = text.find("VALUE").unwrap() as u64;
                *fact = SyntaxFact::new(
                    fact.local_id(),
                    fact.parent(),
                    fact.kind(),
                    SourceSpan::new(fact.span().file(), at, at + 5).unwrap(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                );
            }
            5 => {
                for fact in &mut facts {
                    if fact.local_id() == second_decl.local_id() {
                        *fact = SyntaxFact::new(
                            fact.local_id(),
                            Some(first_decl.local_id()),
                            fact.kind(),
                            fact.span(),
                            first_decl.depth() + 1,
                            fact.syntax_kind().clone(),
                        );
                    } else if fact.parent() == Some(second_decl.local_id()) {
                        *fact = SyntaxFact::new(
                            fact.local_id(),
                            fact.parent(),
                            fact.kind(),
                            fact.span(),
                            first_decl.depth() + 2,
                            fact.syntax_kind().clone(),
                        );
                    }
                }
            }
            _ => facts.retain(|fact| fact.local_id() != context.local_id()),
        }
        if mode == 5 {
            // Disjoint parent extents fail graph validation before lowering.
            assert!(matches!(
                replay(facts),
                Err(AdapterError::ProviderFailed { code })
                    if code.as_str() == "treesitter-lowering-parent"
            ));
            continue;
        }
        let result = replay(facts).unwrap();
        let (bad, good, name, length) = if mode == 5 {
            (second, first, "first", 5)
        } else {
            (first, second, "second", 6)
        };
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == bad),
            "mode {mode}"
        );
        env_definition(&result, name, good, good + length);
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "VALUE")
        );
        assert!(
            !result.document().relations.iter().any(|edge| edge.predicate
                == RelationPredicate::Contains
                && matches!(
                    (edge.subject, edge.object),
                    (RelationEndpoint::Entity(_), RelationEndpoint::Entity(_))
                ))
        );
        assert!(!result.document().skipped_regions.is_empty());
    }
}

#[test]
fn env_original_required_artifact_exact_one_less_and_rebound_source_are_truthful() {
    let fixture = Fixture::new(DOCKERFILE, ENV_ORIGINAL);
    let native = Arc::new(provider());
    let frontend = analyzer(&native, DOCKERFILE);
    let wide = limits();
    let input = request(&fixture.snapshot, &fixture.source, DOCKERFILE, &wide);
    let required = native
        .required_syntax_fact_count(&input.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 6);
    let exact = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, DOCKERFILE, &exact);
    let (first, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    env_no_nested_assignment(
        &first,
        env_definition(&first, "HOST", 206, 210),
        env_definition(&first, "PORT", 224, 228),
    );
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let next = fixture.next_generation();
    let input = request(&next.snapshot, &next.source, DOCKERFILE, &exact);
    let rebound = frontend
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&frontend, &input, &ExtensionSupport::default());
    assert_eq!(rebound.document(), fresh.document());
    assert!(
        rebound
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.generation() == next.source.generation()
                && site.source.content_hash() == content_hash(ENV_ORIGINAL))
    );
    let below = limits_with_syntax_records(required - 1);
    let input = request(&fixture.snapshot, &fixture.source, DOCKERFILE, &below);
    for error in [
        frontend.prepare_artifact(&input, &deadline()).unwrap_err(),
        rootlight_adapter_sdk::execute_parse(
            native.as_ref(),
            &input.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err(),
    ] {
        assert!(
            matches!(error,AdapterError::Sink(SinkError::StreamLimit{resource:ResourceKind::RequiredSyntaxFacts,observed,limit}) if observed==required&&limit==required-1),
            "{error:?}"
        );
    }
    let changed = fixture.rewrite(&[ENV_ORIGINAL, b"\n"].concat());
    assert!(
        matches!(frontend.analyze_from_artifact(&request(&changed.snapshot,&changed.source,DOCKERFILE,&wide),&artifact,ExtensionSupport::default(),MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,&deadline()),Err(AdapterError::ProviderFailed{code}) if code.as_str()=="treesitter-structural-artifact-mismatch")
    );
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        frontend.analyze_from_artifact(
            &request(&fixture.snapshot, &fixture.source, DOCKERFILE, &wide),
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel
        ),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn env_declarations_keep_written_stage_ownership_and_stop_at_unnamed_from_boundary() {
    let bytes =
        b"FROM scratch AS authored\nENV Alpha=one Beta2=two\nFROM scratch\nENV Alpha=again\n";
    let text = std::str::from_utf8(bytes).unwrap();
    let result = output(bytes, EncodingId::utf8());
    let stage = definition(&result, "authored", 16, 24);
    let first = text.find("Alpha").unwrap() as u64;
    let beta = text.find("Beta2").unwrap() as u64;
    let last = text.rfind("Alpha").unwrap() as u64;
    let a = env_definition(&result, "Alpha", first, first + 5);
    let b = env_definition(&result, "Beta2", beta, beta + 5);
    let c = env_definition(&result, "Alpha", last, last + 5);
    env_no_nested_assignment(&result, a, b);
    assert_ne!(a, c);
    for symbol in [a, b] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(
            entity.container,
            Some(rootlight_ir::ContainerRef::Entity(stage))
        );
        assert_eq!(
            result
                .document()
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(stage)
                    && edge.object == RelationEndpoint::Entity(symbol))
                .count(),
            1
        );
    }
    let entity = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.id == c)
        .unwrap();
    assert_eq!(
        entity.container,
        Some(rootlight_ir::ContainerRef::File(
            result.document().files[0].id
        ))
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(stage)
                && edge.object == RelationEndpoint::Entity(c))
    );
}
