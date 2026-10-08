//! Static Erlang tuple bindings require source ownership at every occurrence.

use super::*;

fn fixture() -> (Fixture, SymbolId) {
    let mut fixture = Fixture::new();
    let caller_file = fixture.primary_file;
    let target_file = fixture.add_file(2, "storage.erl");
    let caller = fixture.add_entity(10, "run/1", caller_file, EntityKind::Function, None);
    let module = fixture.add_entity(11, "storage", target_file, EntityKind::Module, None);
    let target = fixture.add_entity(
        12,
        "fetch/1",
        target_file,
        EntityKind::Function,
        Some(module),
    );
    fixture.add_occurrence(
        20,
        "storage::fetch/1",
        caller_file,
        OccurrenceRole::CallSite,
        Some(caller),
    );
    fixture.add_occurrence(
        21,
        "storage::fetch/1",
        caller_file,
        OccurrenceRole::CallSite,
        Some(caller),
    );
    for file in &mut fixture.document.files {
        file.language = "erlang".to_owned();
    }
    fixture.document.provenance[0].language = "erlang".to_owned();
    fixture.document.provenance[0].tier = AnalysisTier::TierD;
    for entity in &mut fixture.document.entities {
        entity.language = "erlang".to_owned();
        entity.tier = AnalysisTier::TierD;
        if entity.id == target {
            entity.qualified_name = "storage::fetch/1".to_owned();
        }
        if entity.id == caller {
            entity.evidence.source = Some(source_ref(
                fixture.document.repository,
                fixture.document.generation,
                caller_file,
                fixture.content_hash,
                0,
                32,
            ));
        }
    }
    for occurrence in &mut fixture.document.occurrences {
        occurrence.syntax_kind = "erlang.function.scoped_call".to_owned();
    }
    let outside = &mut fixture.document.occurrences[1];
    outside.source = source_ref(
        fixture.document.repository,
        fixture.document.generation,
        caller_file,
        fixture.content_hash,
        40,
        48,
    );
    outside.evidence.source = Some(outside.source.clone());
    fixture.validate();
    (fixture, target)
}

#[test]
fn repeated_static_calls_recheck_span_ownership_and_charge_each_site() {
    let (fixture, target) = fixture();
    let engine = ResolutionEngine::new(ResolutionLimits::with_work_limit(4, 4).unwrap());
    let cancellation = Cancellation::new();
    assert_eq!(
        engine
            .estimate_work(&fixture.document, &cancellation)
            .unwrap()
            .required,
        4
    );
    let batch = engine.resolve(&fixture.document, &cancellation).unwrap();
    assert_eq!(batch.decisions.len(), 2);
    assert_eq!(
        batch.decisions[0].outcome,
        ResolutionOutcome::Resolved {
            symbol: target,
            confidence: Confidence::new(900).unwrap(),
        }
    );
    assert!(matches!(
        batch.decisions[1].outcome,
        ResolutionOutcome::Candidates { .. }
    ));
    let insufficient = ResolutionEngine::new(ResolutionLimits::with_work_limit(4, 3).unwrap());
    assert!(matches!(
        insufficient.resolve(&fixture.document, &cancellation),
        Err(ResolutionError::WorkLimit { .. })
    ));
}

#[test]
fn a_qualified_hash_without_module_or_caller_evidence_is_not_an_exact_binding() {
    for mutation in 0..3 {
        let (mut fixture, _) = fixture();
        match mutation {
            0 => fixture.document.entities[1].kind = EntityKind::Namespace,
            1 => fixture.document.entities[1].canonical_name = "unrelated".to_owned(),
            _ => fixture.document.occurrences[0].enclosing = None,
        }
        fixture.validate();
        let batch = ResolutionEngine::default()
            .resolve(&fixture.document, &Cancellation::new())
            .unwrap();
        assert!(
            batch
                .decisions
                .iter()
                .all(|decision| !matches!(decision.outcome, ResolutionOutcome::Resolved { .. }))
        );
    }
}
