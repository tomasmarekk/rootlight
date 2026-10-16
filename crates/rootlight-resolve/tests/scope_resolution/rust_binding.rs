//! Rust lexical declarations require their producer's scope and activation proof.

use super::*;

fn direct_call_fixture() -> (Fixture, SymbolId) {
    let mut fixture = Fixture::new();
    let file = fixture.primary_file;
    let target = fixture.add_entity(10, "target", file, EntityKind::Function, None);
    fixture.add_occurrence(20, "target", file, OccurrenceRole::Definition, None);
    let definition = &mut fixture.document.occurrences[0];
    definition.syntax_kind = "rust.function.definition".to_owned();
    definition.target = OccurrenceTarget::Resolved { symbol: target };
    definition.source = source_ref(
        fixture.document.repository,
        fixture.document.generation,
        file,
        fixture.content_hash,
        0,
        6,
    );
    definition.evidence.source = Some(definition.source.clone());
    fixture.add_occurrence(21, "target", file, OccurrenceRole::CallSite, None);
    fixture.document.occurrences[1].syntax_kind = "rust.direct_call.call".to_owned();
    attach_direct_proof(&mut fixture, target, content_hash(b"target"));
    fixture.validate();
    (fixture, target)
}

fn attach_direct_proof(fixture: &mut Fixture, target: SymbolId, canonical_name: ContentHash) {
    let proof = rootlight_ir::RustDirectCall {
        target,
        definition: fixture.document.occurrences[0].source.span(),
        canonical_name,
    };
    fixture.document.extensions.push(
        rootlight_ir::new_rust_direct_call_envelope(
            fixture.document.repository,
            fixture.document.generation,
            fixture.provenance,
            fixture.document.occurrences[1].source.clone(),
            proof,
        )
        .unwrap(),
    );
}

fn assert_direct_call_unresolved(fixture: &Fixture) {
    let original = fixture
        .document
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::CallSite)
        .unwrap();
    assert!(matches!(
        original.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    let document = ResolutionEngine::default()
        .apply_document(
            fixture.document.clone(),
            ResolverFactContext::new(fixture.content_hash),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(
        document
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::CallSite)
            .unwrap(),
        original
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

fn markdown_direct_call_fixture(nested: bool) -> (Fixture, SymbolId, SymbolId) {
    let (mut fixture, target) = direct_call_fixture();
    fixture.document.files[0].language = "markdown".to_owned();
    fixture.document.provenance[0].language = "markdown".to_owned();
    fixture.document.provenance[0].tier = AnalysisTier::TierD;
    let block = fixture.add_entity(
        12,
        "<code-block>",
        fixture.primary_file,
        EntityKind::Module,
        None,
    );
    let extent = source_ref(
        fixture.document.repository,
        fixture.document.generation,
        fixture.primary_file,
        fixture.content_hash,
        0,
        64,
    );
    fixture.document.entities[1].language = "markdown".to_owned();
    fixture.document.entities[1].evidence.source = Some(extent.clone());
    fixture.add_occurrence(
        23,
        "<code-block>",
        fixture.primary_file,
        OccurrenceRole::Definition,
        None,
    );
    let witness = &mut fixture.document.occurrences[2];
    witness.syntax_kind = "markdown.code_block.module".to_owned();
    witness.target = OccurrenceTarget::Resolved { symbol: block };
    witness.source = extent.clone();
    witness.evidence.source = Some(extent);
    let owner = if nested {
        let module = fixture.add_entity(
            13,
            "inner",
            fixture.primary_file,
            EntityKind::Module,
            Some(block),
        );
        fixture.document.entities[2].evidence.source = Some(source_ref(
            fixture.document.repository,
            fixture.document.generation,
            fixture.primary_file,
            fixture.content_hash,
            0,
            48,
        ));
        module
    } else {
        block
    };
    for entity in &mut fixture.document.entities {
        entity.tier = AnalysisTier::TierD;
    }
    fixture.document.entities[0].container = Some(ContainerRef::Entity(owner));
    fixture.document.occurrences[1].enclosing = Some(owner);
    fixture.validate();
    (fixture, target, block)
}

#[test]
fn markdown_direct_proof_retains_exact_example_witness_and_host_tier() {
    for nested in [false, true] {
        let (fixture, target, block) = markdown_direct_call_fixture(nested);
        let witness = fixture.document.occurrences[2].id;
        let extent = fixture.document.occurrences[2].source.clone();
        let applied = ResolutionEngine::default()
            .apply_document(
                fixture.document,
                ResolverFactContext::new(fixture.content_hash),
                &Cancellation::new(),
            )
            .unwrap();
        let call = applied
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::CallSite)
            .unwrap();
        assert_eq!(call.target, OccurrenceTarget::Resolved { symbol: target });
        let producer = applied
            .provenance
            .iter()
            .find(|record| record.id == call.provenance)
            .unwrap();
        assert_eq!(producer.language, "markdown");
        assert_eq!(producer.tier, AnalysisTier::TierD);
        assert!(
            producer
                .derivation_parents
                .contains(&FactRef::Entity(block))
        );
        assert!(
            producer
                .derivation_parents
                .contains(&FactRef::Fact(witness))
        );
        assert!(producer.input_sources.contains(&extent));
        assert_eq!(
            applied
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Calls)
                .count(),
            1
        );
    }
}

#[test]
fn markdown_module_spelling_without_exact_native_witness_is_insufficient() {
    for mutation in 0..8 {
        let (mut fixture, _, _) = markdown_direct_call_fixture(false);
        match mutation {
            0 => {
                fixture.document.occurrences.pop();
            }
            1 => fixture.document.occurrences[2].role = OccurrenceRole::Declaration,
            2 => fixture.document.occurrences[2].syntax_kind = "markdown.module".to_owned(),
            3 => fixture.document.entities[1].canonical_name = "prose".to_owned(),
            4 => {
                let witness = &mut fixture.document.occurrences[2];
                witness.source = source_ref(
                    fixture.document.repository,
                    fixture.document.generation,
                    fixture.primary_file,
                    fixture.content_hash,
                    0,
                    48,
                );
                witness.evidence.source = Some(witness.source.clone());
            }
            5 => {
                fixture.document.entities[0].container =
                    Some(ContainerRef::File(fixture.primary_file));
                fixture.document.occurrences[1].enclosing = None;
            }
            6 => fixture.document.entities[1].language = "rust".to_owned(),
            7 => {
                let mut duplicate = fixture.document.occurrences[2].clone();
                duplicate.id = FactId::from_bytes([24; 20]);
                fixture.document.occurrences.push(duplicate);
            }
            _ => unreachable!(),
        }
        fixture.validate();
        assert_direct_call_unresolved(&fixture);
    }
}

#[test]
fn markdown_direct_proof_cannot_cross_examples_or_escape_nested_owner() {
    for nested in [false, true] {
        let (mut fixture, _, _) = markdown_direct_call_fixture(nested);
        let other = fixture.add_entity(
            14,
            "<code-block>",
            fixture.primary_file,
            EntityKind::Module,
            None,
        );
        let entity = fixture.document.entities.last_mut().unwrap();
        entity.language = "markdown".to_owned();
        entity.evidence.source = Some(fixture.document.occurrences[2].source.clone());
        let mut witness = fixture.document.occurrences[2].clone();
        witness.id = FactId::from_bytes([25; 20]);
        witness.target = OccurrenceTarget::Resolved { symbol: other };
        fixture.document.occurrences.push(witness);
        fixture.document.occurrences[1].enclosing = Some(other);
        fixture.validate();
        assert_direct_call_unresolved(&fixture);
        if nested {
            fixture.document.entities[2].container = Some(ContainerRef::File(fixture.primary_file));
            fixture.document.occurrences[1].enclosing = Some(fixture.document.entities[2].id);
            fixture.validate();
            assert_direct_call_unresolved(&fixture);
        }
    }
}

#[test]
fn direct_proof_emits_resolver_owned_call_with_exact_witnesses() {
    let (fixture, target) = direct_call_fixture();
    let original = fixture.document.clone();
    let proof_id = original.extensions[0].id;
    let definition_id = original.occurrences[0].id;
    let engine = ResolutionEngine::default();
    let cancellation = Cancellation::new();
    let applied = engine
        .apply_document(
            fixture.document,
            ResolverFactContext::new(fixture.content_hash),
            &cancellation,
        )
        .unwrap();
    let call = applied
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::CallSite)
        .unwrap();
    assert_eq!(call.target, OccurrenceTarget::Resolved { symbol: target });
    let producer = applied
        .provenance
        .iter()
        .find(|producer| producer.id == call.provenance)
        .unwrap();
    assert_eq!(producer.producer_kind, ProducerKind::Rule);
    assert_eq!(
        producer.rule.as_deref(),
        Some("rust-direct-function-v1.written_module")
    );
    assert!(
        producer
            .derivation_parents
            .contains(&FactRef::Fact(proof_id))
    );
    assert!(
        producer
            .derivation_parents
            .contains(&FactRef::Fact(definition_id))
    );
    assert!(
        producer
            .input_sources
            .contains(&original.occurrences[0].source)
    );
    assert!(
        producer
            .input_sources
            .contains(&original.occurrences[1].source)
    );
    assert_eq!(
        applied
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls
                && edge.object == RelationEndpoint::Entity(target))
            .count(),
        1
    );
    let repeated = engine
        .apply_document(
            applied.clone(),
            ResolverFactContext::new(fixture.content_hash),
            &cancellation,
        )
        .unwrap();
    assert_eq!(
        repeated, applied,
        "repeated resolution preserves source-owned provenance"
    );
}

#[test]
fn native_identifier_definitions_join_only_the_proven_function() {
    let (mut fixture, target) = direct_call_fixture();
    fixture.document.occurrences[0].syntax_kind = "rust.identifier.definition".to_owned();
    fixture.validate();
    let batch = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .unwrap();
    assert!(
        matches!(batch.decisions[0].outcome, ResolutionOutcome::Resolved { symbol, .. } if symbol == target)
    );
    fixture.document.entities[0].kind = EntityKind::Variable;
    fixture.validate();
    assert_direct_call_unresolved(&fixture);
}

#[test]
fn raw_and_normalized_authored_hashes_remain_distinct_from_proof_identity() {
    for (definition_spelling, authored, canonical) in [
        ("target", "r#target", "target"),
        ("ta\u{301}rget", "tárget", "tárget"),
    ] {
        let (mut fixture, target) = direct_call_fixture();
        fixture.document.entities[0].canonical_name = canonical.to_owned();
        fixture.document.entities[0].display_name = definition_spelling.to_owned();
        fixture.document.occurrences[0].syntactic_text_hash =
            content_hash(definition_spelling.as_bytes());
        fixture.document.occurrences[1].syntactic_text_hash = content_hash(authored.as_bytes());
        fixture.document.occurrences[1].target = OccurrenceTarget::Unresolved {
            text_hash: content_hash(authored.as_bytes()),
        };
        fixture.document.extensions.clear();
        attach_direct_proof(&mut fixture, target, content_hash(canonical.as_bytes()));
        fixture.validate();
        let batch = ResolutionEngine::default()
            .resolve(&fixture.document, &Cancellation::new())
            .unwrap();
        assert!(
            matches!(batch.decisions[0].outcome, ResolutionOutcome::Resolved { symbol, .. } if symbol == target)
        );
        assert_eq!(
            fixture.document.occurrences[1].syntactic_text_hash,
            content_hash(authored.as_bytes())
        );
    }
}

#[test]
fn written_candidate_targets_are_not_replaced_by_a_direct_claim() {
    let (mut fixture, target) = direct_call_fixture();
    fixture.document.occurrences[1].target = OccurrenceTarget::Candidates {
        symbols: vec![target],
        total_count: 1,
        completeness: CoverageStatus::Bounded,
    };
    let original = fixture.document.occurrences[1].clone();
    fixture.validate();
    let document = ResolutionEngine::default()
        .apply_document(
            fixture.document,
            ResolverFactContext::new(fixture.content_hash),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(
        document
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::CallSite)
            .unwrap(),
        &original
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn missing_or_non_identifier_proof_cannot_resolve_by_spelling() {
    for syntax in [
        "rust.direct_call.call",
        "rust.call.call",
        "rust.scoped_call.scoped_call",
    ] {
        let (mut fixture, _) = direct_call_fixture();
        fixture.document.occurrences[1].syntax_kind = syntax.to_owned();
        if syntax == "rust.direct_call.call" {
            fixture.document.extensions.clear();
        }
        fixture.validate();
        assert_direct_call_unresolved(&fixture);
    }
}

#[test]
fn exact_definition_and_canonical_name_are_required_for_adoption() {
    for mutation in 0..4 {
        let (mut fixture, _) = direct_call_fixture();
        match mutation {
            0 => {
                fixture.document.extensions.clear();
                let target = fixture.document.entities[0].id;
                attach_direct_proof(&mut fixture, target, content_hash(b"other"));
            }
            1 => fixture.document.occurrences[0].role = OccurrenceRole::Declaration,
            2 => {
                fixture.document.occurrences[0].syntax_kind =
                    "rust.local_variable.definition".to_owned()
            }
            3 => fixture.document.entities[0].kind = EntityKind::Method,
            _ => unreachable!(),
        }
        fixture.validate();
        assert_direct_call_unresolved(&fixture);
    }
}

#[test]
fn conflicting_direct_claims_preserve_ambiguity() {
    let (mut fixture, _) = direct_call_fixture();
    let other = fixture.add_entity(
        11,
        "target",
        fixture.primary_file,
        EntityKind::Function,
        None,
    );
    let mut definition = fixture.document.occurrences[0].clone();
    definition.id = FactId::from_bytes([22; 20]);
    definition.target = OccurrenceTarget::Resolved { symbol: other };
    fixture.document.occurrences.push(definition);
    attach_direct_proof(&mut fixture, other, content_hash(b"target"));
    fixture.validate();
    assert_direct_call_unresolved(&fixture);
}

#[test]
fn same_file_proof_does_not_cross_written_module_owners() {
    let (mut fixture, _) = direct_call_fixture();
    let left = fixture.add_entity(12, "left", fixture.primary_file, EntityKind::Module, None);
    let right = fixture.add_entity(13, "right", fixture.primary_file, EntityKind::Module, None);
    fixture.document.entities[0].container = Some(ContainerRef::Entity(left));
    fixture.document.occurrences[1].enclosing = Some(right);
    for entity in &mut fixture.document.entities[1..] {
        entity.evidence.source = Some(source_ref(
            fixture.document.repository,
            fixture.document.generation,
            fixture.primary_file,
            fixture.content_hash,
            0,
            64,
        ));
    }
    fixture.validate();
    assert_direct_call_unresolved(&fixture);
    fixture.document.occurrences[1].enclosing = Some(left);
    let batch = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .unwrap();
    assert!(matches!(
        batch.decisions[0].outcome,
        ResolutionOutcome::Resolved { .. }
    ));
}

#[test]
fn a_container_link_cannot_move_a_function_outside_its_written_module() {
    let (mut fixture, _) = direct_call_fixture();
    let module = fixture.add_entity(
        14,
        "selected",
        fixture.primary_file,
        EntityKind::Module,
        None,
    );
    fixture.document.entities[1].evidence.source = Some(source_ref(
        fixture.document.repository,
        fixture.document.generation,
        fixture.primary_file,
        fixture.content_hash,
        16,
        64,
    ));
    fixture.document.entities[0].container = Some(ContainerRef::Entity(module));
    fixture.document.occurrences[1].enclosing = Some(module);
    fixture.validate();
    assert_direct_call_unresolved(&fixture);
}

#[test]
fn lexical_declarations_cannot_become_spelling_only_candidates() {
    for (kind, role, syntax) in [
        (
            EntityKind::Variable,
            OccurrenceRole::Definition,
            "rust.local_variable.definition",
        ),
        (
            EntityKind::Variable,
            OccurrenceRole::Declaration,
            "rust.local_variable.declaration",
        ),
        (
            EntityKind::Parameter,
            OccurrenceRole::Definition,
            "rust.parameter.definition",
        ),
        (
            EntityKind::Function,
            OccurrenceRole::Declaration,
            "rust.anonymous_function.declaration",
        ),
    ] {
        let mut fixture = Fixture::new();
        let symbol = fixture.add_entity(10, "binding", fixture.primary_file, kind, None);
        fixture.add_occurrence(20, "binding", fixture.primary_file, role, None);
        fixture.document.occurrences[0].syntax_kind = syntax.to_owned();
        fixture.document.occurrences[0].target = OccurrenceTarget::Resolved { symbol };
        fixture.add_occurrence(
            21,
            "binding",
            fixture.primary_file,
            OccurrenceRole::Read,
            None,
        );
        fixture.validate();

        let engine = ResolutionEngine::default();
        let cancellation = Cancellation::new();
        let batch = engine.resolve(&fixture.document, &cancellation).unwrap();
        assert_eq!(batch.decisions.len(), 1, "{syntax}");
        assert!(
            matches!(
                batch.decisions[0].outcome,
                ResolutionOutcome::Unresolved { .. }
            ),
            "{syntax}"
        );
        assert_eq!(
            batch.decisions[0].explanation.rejected_candidates[0].reason,
            RejectionReason::MissingLexicalEvidence,
            "{syntax}"
        );
        let applied = engine
            .apply_document(
                fixture.document,
                ResolverFactContext::new(fixture.content_hash),
                &cancellation,
            )
            .unwrap();
        assert_eq!(
            applied.occurrences[0].target,
            OccurrenceTarget::Resolved { symbol }
        );
        assert!(matches!(
            applied.occurrences[1].target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
}

#[test]
fn parameters_without_written_declarations_require_binding_proof() {
    let mut fixture = Fixture::new();
    fixture.add_entity(
        10,
        "binding",
        fixture.primary_file,
        EntityKind::Parameter,
        None,
    );
    fixture.add_occurrence(
        20,
        "binding",
        fixture.primary_file,
        OccurrenceRole::Read,
        None,
    );
    fixture.validate();
    let batch = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .unwrap();
    assert!(matches!(
        batch.decisions[0].outcome,
        ResolutionOutcome::Unresolved { .. }
    ));
    assert_eq!(
        batch.decisions[0].explanation.rejected_candidates[0].reason,
        RejectionReason::MissingLexicalEvidence
    );
}

#[test]
fn ordinary_item_variables_remain_generic_candidates() {
    let mut fixture = Fixture::new();
    let symbol = fixture.add_entity(10, "ITEM", fixture.primary_file, EntityKind::Variable, None);
    fixture.add_occurrence(20, "ITEM", fixture.primary_file, OccurrenceRole::Read, None);
    fixture.validate();
    let batch = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .unwrap();
    assert!(
        matches!(batch.decisions[0].outcome, ResolutionOutcome::Resolved { symbol: target, .. } if target == symbol)
    );
}

#[test]
fn written_candidate_binding_cannot_be_replaced_by_a_same_name_item() {
    let mut fixture = Fixture::new();
    let local = fixture.add_entity(
        10,
        "binding",
        fixture.primary_file,
        EntityKind::Parameter,
        None,
    );
    fixture.add_entity(
        11,
        "binding",
        fixture.primary_file,
        EntityKind::Variable,
        None,
    );
    fixture.add_occurrence(
        20,
        "binding",
        fixture.primary_file,
        OccurrenceRole::Reference,
        None,
    );
    fixture.document.occurrences[0].syntax_kind = "rust.value.reference".to_owned();
    fixture.document.occurrences[0].target = OccurrenceTarget::Candidates {
        symbols: vec![local],
        total_count: 1,
        completeness: CoverageStatus::Complete,
    };
    fixture.validate();
    let original = fixture.document.occurrences[0].clone();
    let applied = ResolutionEngine::default()
        .apply_document(
            fixture.document,
            ResolverFactContext::new(fixture.content_hash),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(applied.occurrences[0], original);
}
