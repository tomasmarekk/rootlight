//! Public source and raw-witness controls for the bounded standalone SML profile.
//! Original SML/WAT tests retain their exact assertions in the separate module.

use super::*;
use rootlight_adapter_sdk::{
    SyntaxFact, SyntaxFactKind, execute_parse, testkit::MockParseProvider,
};

const CASE: LanguageCase = LanguageCase {
    name: "standard-ml",
    path: "written.sml",
    frontend: "sml-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn analyze_bytes(bytes: &[u8]) -> AnalysisOutput {
    let parser = Arc::new(provider());
    let fixture = Fixture::new(CASE, bytes);
    let limits = limits();
    analyze(
        &analyzer(&parser, CASE),
        &request(&fixture.snapshot, &fixture.source, CASE, &limits),
        &ExtensionSupport::default(),
    )
}

fn assert_formal(document: &rootlight_ir::NormalizedIrDocument, name: &str) -> SymbolId {
    let parameters: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == name)
        .collect();
    assert_eq!(parameters.len(), 1, "one source-owned formal");
    let parameter = parameters[0];
    let Some(rootlight_ir::ContainerRef::Entity(owner)) = parameter.container else {
        panic!("formal must belong to its written function")
    };
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == (OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        })
            })
            .count(),
        1
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(owner)
                    && edge.object == RelationEndpoint::Entity(parameter.id)
            })
            .count(),
        1
    );
    assert!(document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Read
            && site.target
                == (OccurrenceTarget::Resolved {
                    symbol: parameter.id,
                })
            && site.enclosing == Some(owner)
    }));
    parameter.id
}

#[test]
fn standalone_profile_binds_renamed_formals_and_publishes_its_incoming_basis_limit() {
    let source = b"fun renamed input = input;\n";
    let result = analyze_bytes(source);
    assert_formal(result.document(), "input");
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.domain == domain
                && gap.reason == SkippedRegionReason::UnsupportedConstruct
                && gap.detail == "sml-standalone-standard-basis-profile-only-incoming-and-project-basis-unqualified"
                && gap.source.span().start_byte() == 0
                && gap.source.span().end_byte() == u64::try_from(source.len()).unwrap()
                && gap.source.content_hash() == content_hash(source)
        }));
    }
    assert!(
        result
            .document()
            .coverage_records
            .iter()
            .any(|coverage| coverage.status == CoverageStatus::Bounded)
    );
}

#[test]
fn standalone_formal_binding_does_not_adopt_constructor_prefixes_nested_binders_or_type_names() {
    for source in [
        "datatype status = value; fun measure value = value;",
        "exception value; fun measure value = value;",
        "open External; fun measure value = value;",
        "structure Environment = struct end; fun measure value = value;",
        "infix 6 custom; fun measure value = value;",
        "fun measure value = (fn value => value) value;",
        "fun measure value = let val local_value = value in local_value end;",
        "fun measure value = value handle value => value;",
        "fun measure (value, other) = value;",
        "fun measure value other = value;",
    ] {
        let result = analyze_bytes(source.as_bytes());
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Parameter),
            "{source}"
        );
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
    }
    let result = analyze_bytes(b"fun measure value : value = value;\n");
    assert!(
        !result.document().occurrences.iter().any(|site| {
            site.role == OccurrenceRole::Read && site.source.span().start_byte() == 20
        }),
        "the return-type name is not a value read"
    );
}

#[test]
fn formal_resolution_requires_the_exact_profile_declaration_definition_header_and_site_pairs() {
    let bytes = include_bytes!("../fixtures/standard_ml/deephandle.sml");
    let native = provider();
    let fixture = Fixture::new(CASE, bytes);
    let limits = limits();
    let request = request(&fixture.snapshot, &fixture.source, CASE, &limits);
    let parsed = execute_parse(
        &native,
        &request.to_parse_request(),
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
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(parser),
            ProducerIdentity::new(
                "sml-standalone-replay",
                "1.0",
                content_hash(b"sml-standalone-replay"),
            )
            .unwrap(),
            language(CASE),
            CASE.frontend,
            content_hash(b"sml-standalone-replay-binary"),
        )
        .unwrap();
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
    };
    let baseline = replay(parsed.facts().to_vec()).unwrap();
    assert_formal(baseline.document(), "i");
    let witnesses: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| {
            matches!(
                fact.syntax_kind().as_str(),
                "sml.closed_standard_basis_context.signature"
                    | "sml.standalone_parameter.declaration"
                    | "sml.formal_name.definition"
                    | "sml.formal_header.signature"
            ) || (fact.span().end_byte() == 17
                && matches!(
                    fact.syntax_kind().as_str(),
                    "sml.standalone_formal_target.signature"
                        | "sml.standalone_formal_read.reference"
                ))
        })
        .collect();
    assert_eq!(witnesses.len(), 6);
    for witness in witnesses {
        for mode in 0..6 {
            let mut facts = parsed.facts().to_vec();
            match mode {
                0 => {
                    facts.retain(|fact| fact.local_id() != witness.local_id());
                    if witness.kind() == SyntaxFactKind::Declaration {
                        for fact in &mut facts {
                            if fact.parent() == Some(witness.local_id()) {
                                *fact = SyntaxFact::new(
                                    fact.local_id(),
                                    witness.parent(),
                                    fact.kind(),
                                    fact.span(),
                                    fact.depth().saturating_sub(1),
                                    fact.syntax_kind().clone(),
                                );
                            }
                        }
                    }
                }
                1 => {
                    let id = facts
                        .iter()
                        .map(SyntaxFact::local_id)
                        .max()
                        .unwrap()
                        .checked_add(1)
                        .unwrap();
                    facts.push(SyntaxFact::new(
                        id,
                        witness.parent(),
                        witness.kind(),
                        witness.span(),
                        witness.depth(),
                        witness.syntax_kind().clone(),
                    ));
                }
                2 => {
                    let slot = facts
                        .iter_mut()
                        .find(|fact| fact.local_id() == witness.local_id())
                        .unwrap();
                    let label = rootlight_adapter_sdk::SyntaxKindLabel::new(match witness.kind() {
                        SyntaxFactKind::Declaration => "wat.parameter.declaration",
                        SyntaxFactKind::Occurrence => "wat.unresolved_name.reference",
                        _ => "wat.header.signature",
                    })
                    .unwrap();
                    *slot = SyntaxFact::new(
                        witness.local_id(),
                        witness.parent(),
                        witness.kind(),
                        witness.span(),
                        witness.depth(),
                        label,
                    );
                }
                3 => {
                    let slot = facts
                        .iter_mut()
                        .find(|fact| fact.local_id() == witness.local_id())
                        .unwrap();
                    *slot = SyntaxFact::new(
                        witness.local_id(),
                        None,
                        witness.kind(),
                        witness.span(),
                        0,
                        witness.syntax_kind().clone(),
                    );
                }
                4 => {
                    let slot = facts
                        .iter_mut()
                        .find(|fact| fact.local_id() == witness.local_id())
                        .unwrap();
                    let role = match witness.kind() {
                        SyntaxFactKind::Signature => SyntaxFactKind::Occurrence,
                        SyntaxFactKind::Occurrence => SyntaxFactKind::Signature,
                        _ => SyntaxFactKind::Scope,
                    };
                    *slot = SyntaxFact::new(
                        witness.local_id(),
                        witness.parent(),
                        role,
                        witness.span(),
                        witness.depth(),
                        witness.syntax_kind().clone(),
                    );
                }
                _ => {
                    let slot = facts
                        .iter_mut()
                        .find(|fact| fact.local_id() == witness.local_id())
                        .unwrap();
                    let span = SourceSpan::new(
                        witness.span().file(),
                        witness.span().start_byte() + 1,
                        witness.span().end_byte() + u64::from(witness.span().start_byte() != 0),
                    )
                    .unwrap();
                    *slot = SyntaxFact::new(
                        witness.local_id(),
                        witness.parent(),
                        witness.kind(),
                        span,
                        witness.depth(),
                        witness.syntax_kind().clone(),
                    );
                }
            }
            let changed = match replay(facts) {
                Ok(changed) => changed,
                Err(AdapterError::ProviderFailed { code })
                    if matches!(mode, 3 | 5) && code.as_str() == "treesitter-lowering-parent" =>
                {
                    continue;
                }
                Err(error) => panic!("{} mode{mode}: {error:?}", witness.syntax_kind().as_str()),
            };
            if witness.syntax_kind().as_str() == "sml.closed_standard_basis_context.signature" {
                assert!(
                    !changed
                        .document()
                        .entities
                        .iter()
                        .any(|entity| entity.kind == EntityKind::Parameter),
                    "basis mode{mode}"
                );
            }
            assert!(
                !changed.document().occurrences.iter().any(|site| {
                    site.role == OccurrenceRole::Read
                        && site.source.span().start_byte() == 16
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })
                }),
                "{} mode{mode}",
                witness.syntax_kind().as_str()
            );
        }
    }
}

#[test]
fn current_source_type_comment_and_string_tokens_cannot_forge_formal_reads() {
    for source in [
        "fun measure value = if value = 0 then (0 : value) else 1;",
        "fun measure value = if value = 0 then \"value\" else \"\";",
        "fun measure value = if value = 0 then (* value *) 0 else 1;",
        "fun measure value = if value = 0 then {value=0} else {other=1};",
        "fun measure value = if value = 0 then value.member else 1;",
    ] {
        let native = provider();
        let fixture = Fixture::new(CASE, source.as_bytes());
        let limits = limits();
        let request = request(&fixture.snapshot, &fixture.source, CASE, &limits);
        let parsed = execute_parse(
            &native,
            &request.to_parse_request(),
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
            let analyzer = TreeSitterAnalyzer::new(
                Arc::new(parser),
                ProducerIdentity::new(
                    "sml-source-site-replay",
                    "1.0",
                    content_hash(b"sml-source-site-replay"),
                )
                .unwrap(),
                language(CASE),
                CASE.frontend,
                content_hash(b"sml-source-site-replay-binary"),
            )
            .unwrap();
            execute_analysis(
                &analyzer,
                &request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
        };
        let baseline = replay(parsed.facts().to_vec()).unwrap();
        assert_formal(baseline.document(), "value");
        let owner = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.kind() == SyntaxFactKind::Declaration
                    && fact.syntax_kind().as_str() == "sml.function.declaration"
            })
            .unwrap();
        let formal = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.kind() == SyntaxFactKind::Declaration
                    && fact.syntax_kind().as_str() == "sml.standalone_parameter.declaration"
            })
            .unwrap();
        let at = u64::try_from(source.rfind("value").unwrap()).unwrap();
        let mut facts = parsed.facts().to_vec();
        let id = facts
            .iter()
            .map(SyntaxFact::local_id)
            .max()
            .unwrap()
            .checked_add(1)
            .unwrap();
        for (offset, kind, start, label) in [
            (
                0,
                SyntaxFactKind::Occurrence,
                at,
                "sml.standalone_formal_read.reference",
            ),
            (
                1,
                SyntaxFactKind::Signature,
                formal.span().start_byte(),
                "sml.standalone_formal_target.signature",
            ),
        ] {
            facts.push(SyntaxFact::new(
                id + offset,
                Some(owner.local_id()),
                kind,
                SourceSpan::new(fixture.source.span().file(), start, at + 5).unwrap(),
                owner.depth() + 1,
                rootlight_adapter_sdk::SyntaxKindLabel::new(label).unwrap(),
            ));
        }
        let changed = replay(facts).unwrap();
        assert!(
            !changed.document().occurrences.iter().any(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == at
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            }),
            "{source}"
        );
        assert!(changed.document().skipped_regions.iter().any(|gap| {
            gap.detail == "sml-standalone-formal-read-native-binding-witness-unavailable"
                && gap.source.span().start_byte() == at
        }));
    }
}
