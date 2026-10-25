//! Native written impl ownership keeps lexical symbol recipes and exact sources.
//! The complete Deno fixture is shared with the separately guarded field tests.

use super::*;
use rootlight_ir::{
    ContainerRef, SYMBOL_IDENTITY_CLAIM_NAMESPACE, SymbolIdentityClaim,
    decode_symbol_identity_claim_envelope,
};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/rust_named_fields/event_loop.rs");

fn case() -> LanguageCase {
    let mut case = *CASES.iter().find(|case| case.name == "rust").unwrap();
    case.path = "libs/core/event_loop.rs";
    case.generated = false;
    case
}

fn claim(document: &rootlight_ir::NormalizedIrDocument, symbol: SymbolId) -> SymbolIdentityClaim {
    let claims: Vec<_> = document
        .extensions
        .iter()
        .filter(|envelope| envelope.namespace == SYMBOL_IDENTITY_CLAIM_NAMESPACE)
        .map(|envelope| decode_symbol_identity_claim_envelope(envelope).unwrap())
        .filter(|claim| claim.symbol == symbol)
        .collect();
    assert_eq!(claims.len(), 1);
    claims[0].clone()
}

fn entity(document: &rootlight_ir::NormalizedIrDocument, name: &str, kind: EntityKind) -> SymbolId {
    let entities: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name && entity.kind == kind)
        .collect();
    assert_eq!(entities.len(), 1, "{name}");
    entities[0].id
}

fn owned(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
    start: u64,
    end: u64,
) -> SymbolId {
    let symbol = entity(document, name, kind);
    let owner = entity(document, "EventLoopPhases", EntityKind::Struct);
    let record = document
        .entities
        .iter()
        .find(|record| record.id == symbol)
        .unwrap();
    assert_eq!(record.container, Some(ContainerRef::Entity(owner)));
    let claim = claim(document, symbol);
    assert_eq!(claim.container, record.container);
    assert_eq!(claim.derived_symbol(), symbol);
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
    assert_eq!(sites[0].target, OccurrenceTarget::Resolved { symbol });
    assert_eq!(sites[0].enclosing, Some(owner));
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.object == RelationEndpoint::Entity(symbol)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].subject, RelationEndpoint::Entity(owner));
    let evidence = edges[0].evidence.source.as_ref().unwrap();
    assert!(evidence.span().start_byte() <= start && evidence.span().end_byte() >= end);
    assert_eq!(evidence.repository(), sites[0].source.repository());
    assert_eq!(evidence.generation(), sites[0].source.generation());
    assert_eq!(evidence.content_hash(), sites[0].source.content_hash());
    assert_eq!(evidence.span().file(), sites[0].source.span().file());
    symbol
}

#[test]
fn complete_original_deno_source_keeps_exact_fields_method_targets_and_struct_ownership() {
    assert_eq!(ORIGINAL.len(), 1724);
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case(), ORIGINAL);
    let limits = limits();
    let output = analyze(
        &analyzer(&provider, case()),
        &request(&fixture.snapshot, &fixture.source, case(), &limits),
        &ExtensionSupport::default(),
    );
    validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    let owner = entity(output.document(), "EventLoopPhases", EntityKind::Struct);
    let definitions: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 1104
                && site.source.span().end_byte() == 1119
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    assert_eq!(
        definitions[0].target,
        OccurrenceTarget::Resolved { symbol: owner }
    );
    for (name, kind, start, end) in [
        ("close_callbacks", EntityKind::Field, 1160, 1175),
        ("run_close_callbacks", EntityKind::Method, 1392, 1411),
        ("drain_v8_close_callbacks", EntityKind::Method, 1608, 1632),
    ] {
        assert_eq!(
            &ORIGINAL[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
            name.as_bytes()
        );
        owned(output.document(), name, kind, start, end);
    }
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .all(|site| site.role != OccurrenceRole::Definition
                || site.source.span().start_byte() != 1455
                || site.source.span().end_byte() != 1470)
    );
}

#[test]
fn source_above_identity_signature_ceiling_keeps_original_native_owner_ranges() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, case());
    let fixture = Fixture::new(case(), ORIGINAL);
    let mut bytes = ORIGINAL.to_vec();
    bytes.extend_from_slice(format!("\n/*{}*/\n", "padding ".repeat(600)).as_bytes());
    assert!(bytes.len() > rootlight_ir::MAX_LEXICAL_SIGNATURE_BYTES);
    let changed = fixture.rewrite(&bytes);
    let limits = limits();
    let current = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, case(), &limits),
        &ExtensionSupport::default(),
    );
    owned(
        current.document(),
        "run_close_callbacks",
        EntityKind::Method,
        1392,
        1411,
    );
    owned(
        current.document(),
        "drain_v8_close_callbacks",
        EntityKind::Method,
        1608,
        1632,
    );
}

#[test]
fn added_forward_or_moved_struct_preserves_method_and_descendant_identity_recipes() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, case());
    let limits = limits();
    let absent = "impl EventLoopPhases { fn run() { let local = 1; } }\n";
    let fixture = Fixture::new(case(), absent.as_bytes());
    let before = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, case(), &limits),
        &ExtensionSupport::default(),
    );
    let method = entity(before.document(), "run", EntityKind::Method);
    let local = entity(before.document(), "local", EntityKind::Variable);
    let recipe = claim(before.document(), method);
    assert!(matches!(recipe.container, Some(ContainerRef::File(_))));
    for text in [
        "struct EventLoopPhases {}\nimpl EventLoopPhases { fn run() { let local = 1; } }\n",
        "impl EventLoopPhases { fn run() { let local = 1; } }\nstruct EventLoopPhases {}\n",
    ] {
        let changed = fixture.rewrite(text.as_bytes());
        let request = request(&changed.snapshot, &changed.source, case(), &limits);
        let (current, artifact) = analyzer
            .analyze_and_capture(
                &request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let start = text.find("run").unwrap() as u64;
        assert_eq!(
            owned(
                current.document(),
                "run",
                EntityKind::Method,
                start,
                start + 3
            ),
            method
        );
        assert_eq!(
            entity(current.document(), "local", EntityKind::Variable),
            local
        );
        let projected = claim(current.document(), method);
        assert_eq!(projected.container_identity, recipe.container_identity);
        assert_eq!(
            projected.signature_discriminator,
            recipe.signature_discriminator
        );
        assert_eq!(
            projected.build_context_discriminator,
            recipe.build_context_discriminator
        );
        let next = changed.next_generation();
        let next_request = super::request(&next.snapshot, &next.source, case(), &limits);
        let reused = analyzer
            .analyze_from_artifact(
                &next_request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            owned(
                reused.document(),
                "run",
                EntityKind::Method,
                start,
                start + 3
            ),
            method
        );
        assert_eq!(
            claim(reused.document(), method).container_identity,
            recipe.container_identity
        );
        assert!(
            reused
                .document()
                .occurrences
                .iter()
                .all(|site| site.source.generation() == next.source.generation())
        );
    }
}

#[test]
fn ambiguous_or_unwritten_owner_never_changes_a_retained_methods_lexical_parent() {
    for source in [
        "// struct EventLoopPhases {}\nimpl EventLoopPhases { fn run() {} }\n",
        "struct EventLoopPhases {}\nstruct EventLoopPhases { other: u8 }\nimpl EventLoopPhases { fn run() {} }\n",
        "struct EventLoopPhases {}\nuse other::EventLoopPhases;\nimpl EventLoopPhases { fn run() {} }\n",
        "#[cfg(any())]\nstruct EventLoopPhases {}\nimpl EventLoopPhases { fn run() {} }\n",
        "struct EventLoopPhases<T>(T);\nimpl<T> EventLoopPhases<T> { fn run() {} }\n",
    ] {
        let provider = Arc::new(provider());
        let fixture = Fixture::new(case(), source.as_bytes());
        let limits = limits();
        let current = analyze(
            &analyzer(&provider, case()),
            &request(&fixture.snapshot, &fixture.source, case(), &limits),
            &ExtensionSupport::default(),
        );
        let method = entity(current.document(), "run", EntityKind::Method);
        assert!(matches!(
            claim(current.document(), method).container,
            Some(ContainerRef::File(_))
        ));
        assert!(
            current
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "rust-inherent-impl-owner-unavailable")
        );
    }
}

#[test]
fn service_unbounded_syntax_policy_keeps_original_deno_owners_in_fresh_and_prepared_analysis() {
    let provider = Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                64 * 1024 * 1024,
                usize::MAX,
                4096,
                32,
                64,
                1,
                16 * 1024 * 1024,
                ParserSettings::new(4096).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    assert_eq!(provider.capabilities().max_syntax_nodes(), usize::MAX);
    let analyzer = TreeSitterAnalyzer::new_rust_structural(
        provider,
        producer_identity(),
        language(case()),
        case().frontend,
        content_hash(BINARY_SEED),
    )
    .unwrap();
    let fixture = Fixture::new(case(), ORIGINAL);
    let ordinary = limits();
    let service_limits = rootlight_adapter_sdk::AnalysisLimits::new(
        64 * 1024 * 1024,
        usize::MAX,
        4096,
        ordinary.max_embedded_ranges(),
        ordinary.max_reported_memory_bytes(),
        ordinary.syntax_stream().clone(),
        ordinary.ir_stream().clone(),
        ordinary.ir().clone(),
    )
    .unwrap();
    let request = rootlight_adapter_sdk::AnalysisRequest::new_with_parse_context(
        rootlight_adapter_sdk::GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source)
            .unwrap(),
        language(case()),
        rootlight_adapter_sdk::EncodingId::utf8(),
        Vec::new(),
        rootlight_ir::AnalysisTier::TierB,
        build_context(),
        &service_limits,
    )
    .unwrap()
    .with_generated_status(false);
    let direct = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("service-equivalent source/node ceilings admit actual Rust ownership");
    let artifact = analyzer.prepare_artifact(&request, &deadline()).unwrap();
    let prepared = analyzer
        .analyze_prepared_artifact(
            &request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("retained native facts keep ownership under the same configured ceilings");
    assert_eq!(direct.document(), prepared.document());
    for output in [&direct, &prepared] {
        for (name, kind, start, end) in [
            ("close_callbacks", EntityKind::Field, 1160, 1175),
            ("run_close_callbacks", EntityKind::Method, 1392, 1411),
            ("drain_v8_close_callbacks", EntityKind::Method, 1608, 1632),
        ] {
            owned(output.document(), name, kind, start, end);
        }
    }
}
