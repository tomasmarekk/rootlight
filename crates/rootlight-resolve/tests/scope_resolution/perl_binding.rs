//! Canonical Perl evidence selection across producer order and fresh generations.
//! Source order is stable; envelope IDs deliberately include the generation.

use super::*;
use rootlight_ir::{
    PerlBinding, PerlBindingEvidence, PerlCallableStorage, new_perl_binding_envelope,
};

fn fixture(generation: u8, kind: PerlBinding) -> (Fixture, [FactId; 2]) {
    let mut fixture = Fixture::new();
    fixture.document.generation = GenerationId::from_bytes([generation; 20]);
    let repository = fixture.document.repository;
    let generation = fixture.document.generation;
    fixture.document.files[0] = file_record(
        repository,
        generation,
        fixture.primary_file,
        fixture.provenance,
        fixture.content_hash,
        "client.pl",
    );
    let source = source_ref(
        repository,
        generation,
        fixture.primary_file,
        fixture.content_hash,
        0,
        SOURCE_BYTES,
    );
    let provenance = &mut fixture.document.provenance[0];
    provenance.generation = generation;
    provenance.language = "perl".to_owned();
    provenance.input_sources = vec![source.clone()];
    provenance.evidence_sources = vec![source];
    let module = fixture.add_file(9, "module.pm");
    let target = fixture.add_entity(8, "run", module, EntityKind::Function, None);
    fixture.add_occurrence(
        7,
        "Atlas::run",
        fixture.primary_file,
        OccurrenceRole::CallSite,
        None,
    );
    for file in &mut fixture.document.files {
        file.language = "perl".to_owned();
    }
    fixture.document.entities[0].language = "perl".to_owned();
    fixture.document.occurrences[0].syntax_kind = "perl.static_function_name.reference".to_owned();
    if !matches!(kind, PerlBinding::ModuleLoad { .. }) {
        fixture.document.occurrences[0].target = OccurrenceTarget::Resolved { symbol: target };
    }
    let storage = PerlCallableStorage {
        package: content_hash(b"Atlas"),
        name: content_hash(b"run"),
    };
    let claims = [
        (
            fixture.primary_file,
            0,
            SOURCE_BYTES,
            PerlBinding::ModuleContext,
        ),
        (module, 0, SOURCE_BYTES, PerlBinding::ModuleContext),
        (
            module,
            0,
            8,
            PerlBinding::Definition {
                storage,
                symbol: target,
            },
        ),
        (fixture.primary_file, 16, 24, PerlBinding::Call { storage }),
        (fixture.primary_file, 32, 36, kind),
        (fixture.primary_file, 40, 44, kind),
    ];
    for (file, start, end, binding) in claims {
        fixture.document.extensions.push(
            new_perl_binding_envelope(
                repository,
                generation,
                fixture.provenance,
                source_ref(
                    repository,
                    generation,
                    file,
                    fixture.content_hash,
                    start,
                    end,
                ),
                PerlBindingEvidence::new(SourceSpan::new(file, 0, SOURCE_BYTES).unwrap(), binding),
            )
            .unwrap(),
        );
    }
    let ids = [
        fixture.document.extensions[4].id,
        fixture.document.extensions[5].id,
    ];
    fixture.validate();
    (fixture, ids)
}

fn assert_first_source_selected(kind: PerlBinding) {
    for generation in 1..=16 {
        let (fixture, [first, later]) = fixture(generation, kind);
        for reversed in [false, true] {
            let mut document = fixture.document.clone();
            if reversed {
                document.extensions.reverse();
            }
            let resolved = ResolutionEngine::default()
                .apply_document(
                    document,
                    ResolverFactContext::new(content_hash(b"resolver-proof-order")),
                    &Cancellation::new(),
                )
                .unwrap();
            let occurrence = &resolved.occurrences[0];
            let provenance = resolved
                .provenance
                .iter()
                .find(|record| record.id == occurrence.provenance)
                .unwrap();
            assert!(
                provenance
                    .derivation_parents
                    .contains(&FactRef::Fact(first)),
                "generation {generation}, reversed {reversed}: first written source must be selected"
            );
            assert!(
                !provenance
                    .derivation_parents
                    .contains(&FactRef::Fact(later))
            );
            if matches!(kind, PerlBinding::ModuleLoad { .. }) {
                assert!(matches!(
                    occurrence.target,
                    OccurrenceTarget::Resolved { .. }
                ));
            } else {
                assert!(matches!(
                    occurrence.target,
                    OccurrenceTarget::Unresolved { .. }
                ));
            }
        }
    }
}

#[test]
fn repeated_perl_module_loads_select_generation_neutral_source_evidence() {
    assert_first_source_selected(PerlBinding::ModuleLoad {
        package: content_hash(b"Atlas"),
    });
}

#[test]
fn repeated_perl_static_writes_select_generation_neutral_source_evidence() {
    assert_first_source_selected(PerlBinding::Write {
        storage: PerlCallableStorage {
            package: content_hash(b"Atlas"),
            name: content_hash(b"run"),
        },
    });
}

#[test]
fn repeated_perl_dynamic_writes_select_generation_neutral_source_evidence() {
    assert_first_source_selected(PerlBinding::DynamicWrite);
}
