//! Elm local type lookup accepts only a complete source-owned module scope.

use super::*;

const SOURCE: &[u8] = b"module Demo exposing (..)\ntype State = Ready\nview : State\n";

fn fixture() -> (Fixture, SymbolId, SymbolId, SymbolId) {
    let mut fixture = Fixture::new();
    fixture.content_hash = content_hash(SOURCE);
    let file = fixture.primary_file;
    let full = source_ref(
        fixture.document.repository,
        fixture.document.generation,
        file,
        fixture.content_hash,
        0,
        u64::try_from(SOURCE.len()).unwrap(),
    );
    let record = &mut fixture.document.files[0];
    record.path = "Demo.elm".to_owned();
    record.language = "elm".to_owned();
    record.content_hash = fixture.content_hash;
    record.byte_length = u64::try_from(SOURCE.len()).unwrap();
    record.evidence.source = Some(full.clone());
    let provenance = &mut fixture.document.provenance[0];
    provenance.language = "elm".to_owned();
    provenance.input_sources = vec![full.clone()];
    provenance.evidence_sources = vec![full.clone()];
    let module = fixture.add_entity(10, "Demo", file, EntityKind::Module, None);
    let target = fixture.add_entity(11, "State", file, EntityKind::Enum, Some(module));
    let caller = fixture.add_entity(12, "view", file, EntityKind::Function, Some(module));
    let text = std::str::from_utf8(SOURCE).unwrap();
    let target_start = u64::try_from(text.find("type State").unwrap()).unwrap();
    let caller_start = u64::try_from(text.find("view").unwrap()).unwrap();
    for entity in &mut fixture.document.entities {
        entity.language = "elm".to_owned();
        entity.qualified_name = if entity.id == module {
            "Demo".to_owned()
        } else {
            format!("Demo::{}", entity.canonical_name)
        };
        entity.evidence.source = Some(if entity.id == module {
            full.clone()
        } else {
            source_ref(
                fixture.document.repository,
                fixture.document.generation,
                file,
                fixture.content_hash,
                if entity.id == target {
                    target_start
                } else {
                    caller_start
                },
                if entity.id == target {
                    caller_start
                } else {
                    full.span().end_byte()
                },
            )
        });
    }
    fixture.add_occurrence(20, "State", file, OccurrenceRole::TypeUse, Some(caller));
    let start = u64::try_from(text.rfind("State").unwrap()).unwrap();
    let site = &mut fixture.document.occurrences[0];
    site.source = source_ref(
        fixture.document.repository,
        fixture.document.generation,
        file,
        fixture.content_hash,
        start,
        start + 5,
    );
    site.evidence.source = Some(site.source.clone());
    site.syntax_kind = "elm.type.reference".to_owned();
    fixture.validate();
    (fixture, module, target, caller)
}

#[test]
fn module_owned_types_and_legacy_file_types_keep_exact_binding_and_work_admission() {
    for file_owned in [false, true] {
        let (mut fixture, _, target, _) = fixture();
        if file_owned {
            fixture.document.entities[1].container = Some(ContainerRef::File(fixture.primary_file));
        }
        fixture.validate();
        let cancellation = Cancellation::new();
        let engine = ResolutionEngine::new(ResolutionLimits::with_work_limit(4, 2).unwrap());
        assert_eq!(
            engine
                .estimate_work(&fixture.document, &cancellation)
                .unwrap()
                .required,
            2
        );
        let batch = engine.resolve(&fixture.document, &cancellation).unwrap();
        assert_eq!(batch.decisions.len(), 1);
        assert!(matches!(batch.decisions[0].outcome,
            ResolutionOutcome::Resolved { symbol, .. } if symbol == target));
        let applied = engine
            .apply(
                fixture.document.clone(),
                ResolverFactContext::new(content_hash(b"elm-local-module-regression")),
                &cancellation,
            )
            .unwrap();
        let original = &fixture.document.occurrences[0];
        let uses: Vec<_> = applied
            .document
            .occurrences
            .iter()
            .filter(|site| {
                site.source == original.source
                    && site.role == OccurrenceRole::TypeUse
                    && site.syntax_kind == original.syntax_kind
            })
            .collect();
        assert_eq!(uses.len(), 1);
        let resolved = uses[0];
        assert_eq!(
            resolved.target,
            OccurrenceTarget::Resolved { symbol: target }
        );
        assert_eq!(
            resolved.id,
            rootlight_ir::derive_occurrence_record_id(resolved).unwrap()
        );
        assert_ne!(resolved.id, original.id);
        assert!(applied.document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::UsesType
                && relation.subject == RelationEndpoint::Occurrence(resolved.id)
                && relation.object == RelationEndpoint::Entity(target)
                && relation.evidence.source.as_ref()
                    == Some(&fixture.document.occurrences[0].source)
        }));
        assert!(matches!(
            ResolutionEngine::new(ResolutionLimits::with_work_limit(4, 1).unwrap())
                .resolve(&fixture.document, &cancellation),
            Err(ResolutionError::WorkLimit { maximum: 1 })
        ));
        let stopped = Cancellation::new();
        stopped.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            engine.resolve(&fixture.document, &stopped),
            Err(ResolutionError::Cancelled(_))
        ));
    }
}

#[test]
fn unproved_or_partial_module_owners_never_promote_a_local_type() {
    // Each mutant retains a valid normalized graph and the exact type/use;
    // only the proposed module ownership authority changes.
    for mutation in 0..8 {
        let (mut fixture, _, _, caller) = fixture();
        let target_source = fixture.document.entities[1]
            .evidence
            .source
            .clone()
            .unwrap();
        match mutation {
            0 => fixture.document.entities[0].kind = EntityKind::Namespace,
            1 => fixture.document.entities[0]
                .flags
                .push(EntityFlag::Synthetic),
            2 => {
                fixture.document.entities[0].container =
                    Some(ContainerRef::Repository(fixture.document.repository))
            }
            3 => fixture.document.entities[0]
                .evidence
                .derivation
                .push(FactRef::Entity(caller)),
            4 => {
                fixture.document.entities[0].evidence.source = Some(source_ref(
                    fixture.document.repository,
                    fixture.document.generation,
                    fixture.primary_file,
                    fixture.content_hash,
                    0,
                    target_source.span().end_byte(),
                ));
            }
            5 => {
                fixture.document.entities[0].evidence.source = Some(source_ref(
                    fixture.document.repository,
                    fixture.document.generation,
                    fixture.primary_file,
                    fixture.content_hash,
                    target_source.span().start_byte(),
                    u64::try_from(SOURCE.len()).unwrap(),
                ))
            }
            6 => fixture.document.entities[0].language = "rust".to_owned(),
            _ => {
                let other = fixture.add_file(2, "Other.elm");
                let other_record = fixture.document.files.last_mut().unwrap();
                other_record.language = "elm".to_owned();
                other_record.byte_length = u64::try_from(SOURCE.len()).unwrap();
                other_record.evidence.source = Some(source_ref(
                    fixture.document.repository,
                    fixture.document.generation,
                    other,
                    fixture.content_hash,
                    0,
                    u64::try_from(SOURCE.len()).unwrap(),
                ));
                let module = &mut fixture.document.entities[0];
                module.container = Some(ContainerRef::File(other));
                module.evidence.source = Some(source_ref(
                    fixture.document.repository,
                    fixture.document.generation,
                    other,
                    fixture.content_hash,
                    0,
                    u64::try_from(SOURCE.len()).unwrap(),
                ));
            }
        }
        fixture.validate();
        let batch = ResolutionEngine::default()
            .resolve(&fixture.document, &Cancellation::new())
            .unwrap();
        assert!(
            matches!(
                batch.decisions[0].outcome,
                ResolutionOutcome::Unresolved { .. }
            ),
            "mutant {mutation}"
        );
    }
}

#[test]
fn module_owned_type_duplicates_and_qualified_uses_preserve_uncertainty() {
    let (mut fixture, module, _, _) = fixture();
    fixture.add_entity(
        13,
        "State",
        fixture.primary_file,
        EntityKind::TypeAlias,
        Some(module),
    );
    let original = fixture.document.entities[1].clone();
    let duplicate = fixture.document.entities.last_mut().unwrap();
    duplicate.language = "elm".to_owned();
    duplicate.qualified_name = "Demo::State".to_owned();
    duplicate.evidence.source = original.evidence.source;
    fixture.validate();
    let engine = ResolutionEngine::default();
    let batch = engine
        .resolve(&fixture.document, &Cancellation::new())
        .unwrap();
    assert!(matches!(
        batch.decisions[0].outcome,
        ResolutionOutcome::Candidates {
            total_count: 2,
            completeness: CoverageStatus::Complete,
            ..
        }
    ));
    fixture.document.occurrences[0].syntax_kind = "elm.ambiguous_type.reference".to_owned();
    fixture.validate();
    assert!(
        engine
            .resolve(&fixture.document, &Cancellation::new())
            .unwrap()
            .decisions
            .is_empty()
    );
    fixture.document.occurrences[0].syntax_kind = "elm.type.reference".to_owned();
    let hash = content_hash(b"Other.State");
    fixture.document.occurrences[0].syntactic_text_hash = hash;
    fixture.document.occurrences[0].target = OccurrenceTarget::Unresolved { text_hash: hash };
    fixture.validate();
    let batch = engine
        .resolve(&fixture.document, &Cancellation::new())
        .unwrap();
    assert!(matches!(
        batch.decisions[0].outcome,
        ResolutionOutcome::Unresolved { .. }
    ));
}

#[test]
fn module_source_hash_and_generation_cannot_borrow_current_file_authority() {
    for stale_generation in [false, true] {
        let (mut fixture, _, _, _) = fixture();
        let module = &mut fixture.document.entities[0];
        module.evidence.source = Some(SourceRef::new(
            fixture.document.repository,
            if stale_generation {
                GenerationId::from_bytes([99; 20])
            } else {
                fixture.document.generation
            },
            SourceSpan::new(
                fixture.primary_file,
                0,
                u64::try_from(SOURCE.len()).unwrap(),
            )
            .unwrap(),
            if stale_generation {
                fixture.content_hash
            } else {
                content_hash(b"stale module bytes")
            },
            None,
        ));
        assert!(matches!(
            ResolutionEngine::default().resolve(&fixture.document, &Cancellation::new()),
            Err(ResolutionError::InvalidDocument(_))
        ));
    }
}
