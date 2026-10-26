//! Module Reference certificates require current native proof at every tier.
//! Spelling scores cannot promote an unresolved or candidate-only private label;
//! already proved targets and ordinary Bash lookup keep their prior contracts.

use super::*;

#[test]
fn bash_companion_labels_refuse_spelling_recovery_at_low_and_high_tiers() {
    for tier in [AnalysisTier::TierD, AnalysisTier::TierA] {
        for syntax in [
            "bash.module_reference.reference",
            "bash.unproven_module_reference.reference",
        ] {
            for state in 0..3 {
                let mut fixture = Fixture::new();
                fixture.document.files[0].language = "bash".to_owned();
                fixture.document.files[0].path = "src/module.sh".to_owned();
                fixture.document.provenance[0].language = "bash".to_owned();
                fixture.document.provenance[0].tier = tier;
                let symbol = fixture.add_entity(
                    10,
                    "selected",
                    fixture.primary_file,
                    EntityKind::Variable,
                    None,
                );
                fixture.document.entities[0].language = "bash".to_owned();
                fixture.document.entities[0].tier = tier;
                fixture.add_occurrence(
                    20,
                    "selected",
                    fixture.primary_file,
                    OccurrenceRole::Reference,
                    None,
                );
                let site = &mut fixture.document.occurrences[0];
                site.syntax_kind = syntax.to_owned();
                site.target = match state {
                    0 => OccurrenceTarget::Unresolved {
                        text_hash: site.syntactic_text_hash,
                    },
                    1 => OccurrenceTarget::Candidates {
                        symbols: vec![symbol],
                        total_count: 1,
                        completeness: CoverageStatus::Complete,
                    },
                    _ => OccurrenceTarget::Resolved { symbol },
                };
                let original = site.clone();
                fixture.validate();
                let engine = ResolutionEngine::default();
                let cancel = Cancellation::new();
                assert_eq!(
                    engine
                        .estimate_work(&fixture.document, &cancel)
                        .unwrap()
                        .required,
                    0,
                    "private label must not enter generic scoring: {tier:?} {syntax} {state}"
                );
                assert!(
                    engine
                        .resolve(&fixture.document, &cancel)
                        .unwrap()
                        .decisions
                        .is_empty()
                );
                let applied = engine
                    .apply_document(
                        fixture.document.clone(),
                        ResolverFactContext::new(fixture.content_hash),
                        &cancel,
                    )
                    .unwrap();
                assert_eq!(
                    applied.occurrences.as_slice(),
                    std::slice::from_ref(&original)
                );
                let (bounded, estimate) = engine
                    .apply_document_bounded(
                        fixture.document,
                        ResolverFactContext::new(fixture.content_hash),
                        &cancel,
                    )
                    .unwrap();
                assert_eq!(estimate.required, 0);
                assert_eq!(bounded.occurrences, [original]);
                assert!(bounded.relations.is_empty());
            }
        }
    }
}

#[test]
fn ordinary_bash_reference_keeps_generic_resolution_available() {
    let mut fixture = Fixture::new();
    fixture.document.files[0].language = "bash".to_owned();
    fixture.document.provenance[0].language = "bash".to_owned();
    fixture.document.provenance[0].tier = AnalysisTier::TierD;
    fixture.add_entity(
        10,
        "selected",
        fixture.primary_file,
        EntityKind::Variable,
        None,
    );
    fixture.document.entities[0].language = "bash".to_owned();
    fixture.document.entities[0].tier = AnalysisTier::TierD;
    fixture.add_occurrence(
        20,
        "selected",
        fixture.primary_file,
        OccurrenceRole::Reference,
        None,
    );
    fixture.document.occurrences[0].syntax_kind = "bash.identifier.reference".to_owned();
    fixture.validate();
    assert_eq!(
        ResolutionEngine::default()
            .resolve(&fixture.document, &Cancellation::new())
            .unwrap()
            .decisions
            .len(),
        1
    );
}
