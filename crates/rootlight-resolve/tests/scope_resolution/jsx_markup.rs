//! Generic lexical scoring cannot select written JSX markup identities.
//! These are real resolver decisions over validated records, including a
//! lexical-variable positive and a producer-resolved preservation control.

use super::*;

#[test]
fn lexical_uses_reject_jsx_markup_candidates_but_keep_variables() {
    for language in ["javascript", "typescript"] {
        for kind in [
            EntityKind::MarkupElement,
            EntityKind::MarkupAttribute,
            EntityKind::Variable,
        ] {
            let mut fixture = Fixture::new();
            fixture.document.version = rootlight_ir::NormalizedIrVersion::V1_3;
            fixture.document.files[0].language = language.to_owned();
            fixture.document.provenance[0].language = language.to_owned();
            let enclosing = (kind == EntityKind::MarkupAttribute).then(|| {
                fixture.add_entity(
                    11,
                    "host",
                    fixture.primary_file,
                    EntityKind::MarkupElement,
                    None,
                )
            });
            fixture.add_entity(10, "chosen", fixture.primary_file, kind, enclosing);
            for entity in &mut fixture.document.entities {
                entity.language = language.to_owned();
            }
            fixture.add_occurrence(
                20,
                "chosen",
                fixture.primary_file,
                OccurrenceRole::Reference,
                enclosing,
            );
            fixture.document.occurrences[0].syntax_kind =
                format!("{language}.identifier.reference");
            fixture.validate();
            let output = ResolutionEngine::default()
                .resolve(&fixture.document, &Cancellation::new())
                .unwrap();
            assert_eq!(output.decisions.len(), 1);
            if kind == EntityKind::Variable {
                assert!(!matches!(
                    output.decisions[0].outcome,
                    ResolutionOutcome::Unresolved { .. }
                ));
            } else {
                assert!(matches!(
                    output.decisions[0].outcome,
                    ResolutionOutcome::Unresolved { .. }
                ));
                assert_eq!(
                    output.decisions[0].explanation.rejected_candidates[0].reason,
                    RejectionReason::TargetKindMismatch
                );
            }
        }
    }
}

#[test]
fn unproven_jsx_component_names_cannot_be_repaired_by_spelling() {
    for language in ["javascript", "typescript"] {
        let mut fixture = Fixture::new();
        fixture.document.files[0].language = language.to_owned();
        fixture.document.provenance[0].language = language.to_owned();
        let symbol = fixture.add_entity(
            10,
            "Component",
            fixture.primary_file,
            EntityKind::Function,
            None,
        );
        fixture.document.entities[0].language = language.to_owned();
        for identity in [20, 21] {
            fixture.add_occurrence(
                identity,
                "Component",
                fixture.primary_file,
                OccurrenceRole::Reference,
                None,
            );
        }
        for site in &mut fixture.document.occurrences {
            site.syntax_kind = format!("{language}.jsx_name.reference");
        }
        fixture.document.occurrences[1].target = OccurrenceTarget::Resolved { symbol };
        fixture.validate();
        let output = ResolutionEngine::default()
            .apply_document(
                fixture.document,
                ResolverFactContext::new(fixture.content_hash),
                &Cancellation::new(),
            )
            .unwrap();
        assert!(matches!(
            output.occurrences[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert_eq!(
            output.occurrences[1].target,
            OccurrenceTarget::Resolved { symbol }
        );
    }
}
