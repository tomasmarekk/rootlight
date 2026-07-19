//! Cross-file acceptance fixtures for language-neutral scope resolution.
//!
//! These tests verify exact local binding and explicit ambiguity before
//! adapter-specific semantic evidence is added.

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{
    ContentHash, FactId, FileId, GenerationId, RepositoryId, SymbolId, content_hash,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, Confidence, ContainerRef, CoverageStatus, EntityFlag,
    EntityKind, EntityRecord, EntityVisibility, EvidenceKind, ExtensionSupport, FactEvidence,
    FactRef, FileRecord, IrLimits, NormalizedIrDocument, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, ProducerKind, ProvenanceRecord, RelationEndpoint,
    RelationPredicate, RelationRecord, SourceRef, SourceSpan, validate_ir_document,
};
use rootlight_resolve::{
    CompletenessAssumption, ExpectedResolution, RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION,
    ResolutionError, ResolutionExpectation, ResolutionSignal, ResolverFactContext,
    UnresolvedReason, evaluate_resolution_quality,
};
use rootlight_resolve::{ResolutionEngine, ResolutionLimits, ResolutionOutcome};

const SOURCE_BYTES: u64 = 64;

#[test]
fn resolves_a_unique_same_file_declaration() {
    let mut fixture = Fixture::new();
    let target = fixture.add_entity(
        10,
        "target",
        fixture.primary_file,
        EntityKind::Function,
        None,
    );
    let occurrence = fixture.add_occurrence(
        20,
        "target",
        fixture.primary_file,
        OccurrenceRole::Reference,
        None,
    );
    fixture.validate();

    let batch = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .expect("valid fixture resolves");

    assert_eq!(batch.decisions.len(), 1);
    assert_eq!(batch.decisions[0].occurrence, occurrence);
    assert_eq!(
        batch.decisions[0].outcome,
        ResolutionOutcome::Resolved {
            symbol: target,
            confidence: Confidence::new(900).expect("fixture confidence is valid"),
        }
    );
}

#[test]
fn preserves_cross_file_import_ambiguity() {
    let mut fixture = Fixture::new();
    let first_file = fixture.add_file(2, "src/first.rs");
    let second_file = fixture.add_file(3, "src/second.rs");
    let first = fixture.add_entity(11, "shared", first_file, EntityKind::Module, None);
    let second = fixture.add_entity(12, "shared", second_file, EntityKind::Module, None);
    let occurrence = fixture.add_occurrence(
        21,
        "shared",
        fixture.primary_file,
        OccurrenceRole::ImportUse,
        None,
    );
    fixture.validate();

    let batch = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .expect("valid fixture resolves");

    let mut expected = vec![first, second];
    expected.sort_unstable();
    assert_eq!(batch.decisions.len(), 1);
    assert_eq!(batch.decisions[0].occurrence, occurrence);
    assert_eq!(
        batch.decisions[0].outcome,
        ResolutionOutcome::Candidates {
            symbols: expected,
            total_count: 2,
            completeness: CoverageStatus::Complete,
            confidence: Confidence::new(900).expect("fixture confidence is valid"),
        }
    );
}

#[test]
fn validates_candidate_bounds() {
    assert!(ResolutionLimits::new(0).is_err());
    assert!(ResolutionLimits::new(4_096).is_ok());
    assert!(ResolutionLimits::new(4_097).is_err());
}

#[test]
fn nearest_lexical_scope_wins_without_erasing_shadowed_candidates() {
    let mut fixture = Fixture::new();
    let outer_scope = fixture.add_entity(
        30,
        "outer",
        fixture.primary_file,
        EntityKind::Function,
        None,
    );
    let inner_scope = fixture.add_entity(
        31,
        "inner",
        fixture.primary_file,
        EntityKind::Function,
        Some(outer_scope),
    );
    let _outer_value = fixture.add_entity(
        32,
        "value",
        fixture.primary_file,
        EntityKind::Variable,
        Some(outer_scope),
    );
    let inner_value = fixture.add_entity(
        33,
        "value",
        fixture.primary_file,
        EntityKind::Variable,
        Some(inner_scope),
    );
    fixture.add_occurrence(
        34,
        "value",
        fixture.primary_file,
        OccurrenceRole::Reference,
        Some(inner_scope),
    );
    fixture.validate();

    let batch = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .expect("valid nested scopes resolve");
    let decision = &batch.decisions[0];

    assert_eq!(
        decision.outcome,
        ResolutionOutcome::Resolved {
            symbol: inner_value,
            confidence: Confidence::new(940).expect("fixture confidence is valid"),
        }
    );
    assert_eq!(decision.explanation.candidates.len(), 2);
    assert!(
        decision.explanation.candidates[0]
            .positive_signals
            .contains(&ResolutionSignal::SameScope)
    );
    assert!(
        decision.explanation.candidates[1]
            .positive_signals
            .contains(&ResolutionSignal::AncestorScope { depth: 1 })
    );
}

#[test]
fn truncates_candidates_without_hiding_total_or_order() {
    let mut fixture = Fixture::new();
    for identity in [42, 40, 41] {
        let file = fixture.add_file(identity, &format!("src/{identity}.rs"));
        fixture.add_entity(identity, "shared", file, EntityKind::Module, None);
    }
    fixture.add_occurrence(
        43,
        "shared",
        fixture.primary_file,
        OccurrenceRole::ImportUse,
        None,
    );
    fixture.validate();
    let engine =
        ResolutionEngine::new(ResolutionLimits::new(2).expect("fixture candidate limit is valid"));

    let batch = engine
        .resolve(&fixture.document, &Cancellation::new())
        .expect("valid fixture resolves");
    let ResolutionOutcome::Candidates {
        symbols,
        total_count,
        completeness,
        ..
    } = &batch.decisions[0].outcome
    else {
        panic!("ambiguous import must remain a candidate set");
    };

    let mut expected = vec![
        SymbolId::from_bytes([40; 20]),
        SymbolId::from_bytes([41; 20]),
    ];
    expected.sort_unstable();
    assert_eq!(symbols, &expected);
    assert_eq!(*total_count, 3);
    assert_eq!(*completeness, CoverageStatus::Bounded);
    assert_eq!(batch.decisions[0].explanation.candidates.len(), 2);
}

#[test]
fn decisions_are_independent_of_producer_order() {
    let mut fixture = Fixture::new();
    let second_file = fixture.add_file(51, "src/second.rs");
    fixture.add_entity(50, "shared", fixture.primary_file, EntityKind::Module, None);
    fixture.add_entity(51, "shared", second_file, EntityKind::Module, None);
    fixture.add_occurrence(
        52,
        "shared",
        fixture.primary_file,
        OccurrenceRole::ImportUse,
        None,
    );
    fixture.validate();
    let expected = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .expect("ordered fixture resolves");

    fixture.document.files.reverse();
    fixture.document.entities.reverse();
    fixture.document.occurrences.reverse();
    fixture.validate();
    let shuffled = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .expect("shuffled fixture resolves");

    assert_eq!(shuffled, expected);
}

#[test]
fn reports_bounded_explanations_and_incompatible_rejections() {
    let mut fixture = Fixture::new();
    fixture.add_entity(
        60,
        "dependency",
        fixture.primary_file,
        EntityKind::Function,
        None,
    );
    fixture.add_occurrence(
        61,
        "dependency",
        fixture.primary_file,
        OccurrenceRole::ImportUse,
        None,
    );
    fixture.validate();

    let batch = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .expect("valid fixture resolves");
    let decision = &batch.decisions[0];

    assert_eq!(
        decision.outcome,
        ResolutionOutcome::Unresolved {
            reason: UnresolvedReason::UnsupportedTargetKind,
            confidence: Confidence::new(0).expect("zero confidence is valid"),
        }
    );
    assert_eq!(decision.explanation.provider_name, RESOLVER_PROVIDER_NAME);
    assert_eq!(
        decision.explanation.provider_version,
        RESOLVER_PROVIDER_VERSION
    );
    assert_eq!(decision.explanation.rejected_total, 1);
    assert_eq!(decision.explanation.rejected_candidates.len(), 1);
    assert!(
        decision
            .explanation
            .completeness_assumptions
            .contains(&CompletenessAssumption::NoRepositoryExecution)
    );
}

#[test]
fn validates_input_and_observes_cancellation() {
    let mut fixture = Fixture::new();
    fixture.add_occurrence(
        70,
        "missing",
        fixture.primary_file,
        OccurrenceRole::Reference,
        None,
    );
    fixture.validate();
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));

    assert!(matches!(
        ResolutionEngine::default().resolve(&fixture.document, &cancellation),
        Err(ResolutionError::Cancelled(cancelled))
            if cancelled.reason() == CancellationReason::ClientRequest
    ));

    fixture.document.files.clear();
    assert!(matches!(
        ResolutionEngine::default().resolve(&fixture.document, &Cancellation::new()),
        Err(ResolutionError::InvalidDocument(_))
    ));
}

#[test]
fn language_mismatch_does_not_masquerade_as_a_kind_failure() {
    let mut fixture = Fixture::new();
    let python_file = fixture.add_file(80, "src/provider.py");
    let python_symbol = fixture.add_entity(80, "provider", python_file, EntityKind::Module, None);
    fixture
        .document
        .files
        .iter_mut()
        .find(|file| file.id == python_file)
        .expect("fixture file exists")
        .language = "python".to_owned();
    fixture
        .document
        .entities
        .iter_mut()
        .find(|entity| entity.id == python_symbol)
        .expect("fixture entity exists")
        .language = "python".to_owned();
    fixture.add_occurrence(
        81,
        "provider",
        fixture.primary_file,
        OccurrenceRole::ImportUse,
        None,
    );
    fixture.validate();

    let batch = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .expect("valid fixture resolves");

    assert_eq!(
        batch.decisions[0].outcome,
        ResolutionOutcome::Unresolved {
            reason: UnresolvedReason::MissingDependency,
            confidence: Confidence::new(0).expect("zero confidence is valid"),
        }
    );
    assert_eq!(batch.decisions[0].explanation.rejected_total, 1);
}

#[test]
fn tier_d_evidence_never_becomes_an_exact_semantic_binding() {
    let mut fixture = Fixture::new();
    let symbol = fixture.add_entity(
        90,
        "syntax_only",
        fixture.primary_file,
        EntityKind::Function,
        None,
    );
    fixture.add_occurrence(
        91,
        "syntax_only",
        fixture.primary_file,
        OccurrenceRole::Reference,
        None,
    );
    fixture.document.provenance[0].tier = AnalysisTier::TierD;
    fixture.document.entities[0].tier = AnalysisTier::TierD;
    fixture.validate();

    let batch = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .expect("valid syntax-only fixture resolves conservatively");

    assert_eq!(
        batch.decisions[0].outcome,
        ResolutionOutcome::Candidates {
            symbols: vec![symbol],
            total_count: 1,
            completeness: CoverageStatus::Complete,
            confidence: Confidence::new(399).expect("tier-D ceiling is valid"),
        }
    );
}

#[test]
fn applies_exact_bindings_with_derived_provenance_and_identity() {
    let mut fixture = Fixture::new();
    let target = fixture.add_entity(
        100,
        "target",
        fixture.primary_file,
        EntityKind::Function,
        None,
    );
    let original_occurrence = fixture.add_occurrence(
        101,
        "target",
        fixture.primary_file,
        OccurrenceRole::Reference,
        None,
    );
    let relation_source = source_ref(
        fixture.document.repository,
        fixture.document.generation,
        fixture.primary_file,
        fixture.content_hash,
        16,
        24,
    );
    fixture.document.relations.push(RelationRecord {
        id: FactId::from_bytes([102; 20]),
        repository: fixture.document.repository,
        generation: fixture.document.generation,
        subject: RelationEndpoint::Entity(target),
        predicate: RelationPredicate::DefinesAt,
        object: RelationEndpoint::Occurrence(original_occurrence),
        confidence: Confidence::new(1_000).expect("fixture confidence is valid"),
        evidence_kind: EvidenceKind::Syntax,
        provenance: fixture.provenance,
        evidence: FactEvidence {
            source: Some(relation_source),
            derivation: vec![FactRef::Entity(target), FactRef::Fact(original_occurrence)],
        },
    });
    fixture.validate();

    let applied = ResolutionEngine::default()
        .apply(
            fixture.document,
            ResolverFactContext::new(fixture.content_hash),
            &Cancellation::new(),
        )
        .expect("valid exact binding applies");
    validate_ir_document(
        &applied.document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("resolved IR remains valid");
    let occurrence = &applied.document.occurrences[0];

    assert_ne!(occurrence.id, original_occurrence);
    assert_eq!(
        occurrence.target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    let provenance = applied
        .document
        .provenance
        .iter()
        .find(|provenance| provenance.id == occurrence.provenance)
        .expect("resolved occurrence provenance exists");
    assert_eq!(provenance.producer_kind, ProducerKind::Rule);
    assert_eq!(provenance.rule.as_deref(), Some("scope-v1.lexical_scope"));
    assert!(applied.document.relations.iter().any(|relation| {
        relation.subject == RelationEndpoint::Occurrence(occurrence.id)
            && relation.predicate == RelationPredicate::RefersTo
            && relation.object == RelationEndpoint::Entity(target)
    }));
    assert!(applied.document.relations.iter().any(|relation| {
        relation.subject == RelationEndpoint::Entity(target)
            && relation.predicate == RelationPredicate::DefinesAt
            && relation.object == RelationEndpoint::Occurrence(occurrence.id)
            && relation.id != FactId::from_bytes([102; 20])
            && relation
                .evidence
                .derivation
                .contains(&FactRef::Fact(occurrence.id))
    }));
}

#[test]
fn applies_ambiguous_calls_only_as_dispatch_candidates() {
    let mut fixture = Fixture::new();
    let first = fixture.add_entity(
        110,
        "dispatch",
        fixture.primary_file,
        EntityKind::Function,
        None,
    );
    let second = fixture.add_entity(
        111,
        "dispatch",
        fixture.primary_file,
        EntityKind::Method,
        None,
    );
    fixture.add_occurrence(
        112,
        "dispatch",
        fixture.primary_file,
        OccurrenceRole::CallSite,
        None,
    );
    fixture.validate();

    let applied = ResolutionEngine::default()
        .apply(
            fixture.document,
            ResolverFactContext::new(fixture.content_hash),
            &Cancellation::new(),
        )
        .expect("valid ambiguous call applies");
    let occurrence = &applied.document.occurrences[0];
    let OccurrenceTarget::Candidates {
        symbols,
        total_count,
        completeness,
    } = &occurrence.target
    else {
        panic!("ambiguous call must remain a candidate target");
    };
    let mut expected = vec![first, second];
    expected.sort_unstable();
    assert_eq!(symbols, &expected);
    assert_eq!(*total_count, 2);
    assert_eq!(*completeness, CoverageStatus::Complete);
    assert!(
        !applied
            .document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    let mut dispatch_targets = applied
        .document
        .relations
        .iter()
        .filter_map(|relation| {
            (relation.predicate == RelationPredicate::DispatchCandidate).then_some(relation.object)
        })
        .collect::<Vec<_>>();
    dispatch_targets.sort_unstable();
    let mut expected_targets = expected
        .into_iter()
        .map(RelationEndpoint::Entity)
        .collect::<Vec<_>>();
    expected_targets.sort_unstable();
    assert_eq!(dispatch_targets, expected_targets);
}

#[test]
fn resolves_aliases_and_type_parameters_as_type_candidates() {
    let mut fixture = Fixture::new();
    let alias = fixture.add_entity(
        120,
        "Alias",
        fixture.primary_file,
        EntityKind::TypeAlias,
        None,
    );
    let parameter = fixture.add_entity(
        121,
        "T",
        fixture.primary_file,
        EntityKind::TypeParameter,
        None,
    );
    fixture.add_occurrence(
        122,
        "Alias",
        fixture.primary_file,
        OccurrenceRole::TypeUse,
        None,
    );
    fixture.add_occurrence(
        123,
        "T",
        fixture.primary_file,
        OccurrenceRole::TypeUse,
        None,
    );
    fixture.validate();

    let applied = ResolutionEngine::default()
        .apply(
            fixture.document,
            ResolverFactContext::new(fixture.content_hash),
            &Cancellation::new(),
        )
        .expect("type candidates apply");
    let resolved_targets = applied
        .document
        .occurrences
        .iter()
        .filter_map(|occurrence| {
            if let OccurrenceTarget::Resolved { symbol } = occurrence.target {
                Some(symbol)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    assert!(resolved_targets.contains(&alias));
    assert!(resolved_targets.contains(&parameter));
    assert_eq!(
        applied
            .document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::UsesType)
            .count(),
        2
    );
}

#[test]
fn anchors_hierarchy_relations_on_the_enclosing_type() {
    let mut fixture = Fixture::new();
    let child = fixture.add_entity(130, "Child", fixture.primary_file, EntityKind::Class, None);
    let base = fixture.add_entity(131, "Base", fixture.primary_file, EntityKind::Class, None);
    let interface = fixture.add_entity(
        132,
        "Contract",
        fixture.primary_file,
        EntityKind::Interface,
        None,
    );
    fixture.add_occurrence(
        133,
        "Base",
        fixture.primary_file,
        OccurrenceRole::InheritanceUse,
        Some(child),
    );
    fixture.add_occurrence(
        134,
        "Contract",
        fixture.primary_file,
        OccurrenceRole::ImplementationUse,
        Some(child),
    );
    fixture.validate();

    let applied = ResolutionEngine::default()
        .apply(
            fixture.document,
            ResolverFactContext::new(fixture.content_hash),
            &Cancellation::new(),
        )
        .expect("hierarchy candidates apply");

    assert!(applied.document.relations.iter().any(|relation| {
        relation.subject == RelationEndpoint::Entity(child)
            && relation.predicate == RelationPredicate::Extends
            && relation.object == RelationEndpoint::Entity(base)
    }));
    assert!(applied.document.relations.iter().any(|relation| {
        relation.subject == RelationEndpoint::Entity(child)
            && relation.predicate == RelationPredicate::Implements
            && relation.object == RelationEndpoint::Entity(interface)
    }));
}

#[test]
fn resolves_import_aliases_and_qualified_names() {
    let mut fixture = Fixture::new();
    let module = fixture.add_entity(
        140,
        "canonical_module",
        fixture.primary_file,
        EntityKind::Module,
        None,
    );
    let entity = fixture
        .document
        .entities
        .iter_mut()
        .find(|entity| entity.id == module)
        .expect("fixture module exists");
    entity.display_name = "module_alias".to_owned();
    entity.qualified_name = "workspace::canonical_module".to_owned();
    fixture.add_occurrence(
        141,
        "module_alias",
        fixture.primary_file,
        OccurrenceRole::ImportUse,
        None,
    );
    fixture.add_occurrence(
        142,
        "workspace::canonical_module",
        fixture.primary_file,
        OccurrenceRole::ImportUse,
        None,
    );
    fixture.validate();

    let batch = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .expect("alias fixture resolves");

    assert_eq!(batch.decisions.len(), 2);
    assert!(batch.decisions.iter().all(|decision| {
        matches!(
            decision.outcome,
            ResolutionOutcome::Resolved { symbol, .. } if symbol == module
        )
    }));
}

#[test]
fn scores_direct_ambiguous_and_unresolved_call_outcomes() {
    let mut fixture = Fixture::new();
    let exact_target = fixture.add_entity(
        150,
        "direct",
        fixture.primary_file,
        EntityKind::Function,
        None,
    );
    let first_candidate = fixture.add_entity(
        151,
        "dynamic",
        fixture.primary_file,
        EntityKind::Function,
        None,
    );
    fixture.add_entity(
        152,
        "dynamic",
        fixture.primary_file,
        EntityKind::Method,
        None,
    );
    let direct_call = fixture.add_occurrence(
        153,
        "direct",
        fixture.primary_file,
        OccurrenceRole::CallSite,
        None,
    );
    let ambiguous_call = fixture.add_occurrence(
        154,
        "dynamic",
        fixture.primary_file,
        OccurrenceRole::CallSite,
        None,
    );
    let unresolved_call = fixture.add_occurrence(
        155,
        "reflective",
        fixture.primary_file,
        OccurrenceRole::CallSite,
        None,
    );
    fixture.validate();
    let engine = ResolutionEngine::default();
    let batch = engine
        .resolve(&fixture.document, &Cancellation::new())
        .expect("call fixture resolves");
    let report = evaluate_resolution_quality(
        &batch,
        &[
            ResolutionExpectation {
                occurrence: direct_call,
                expected: ExpectedResolution::Exact(exact_target),
            },
            ResolutionExpectation {
                occurrence: ambiguous_call,
                expected: ExpectedResolution::CandidateContains(first_candidate),
            },
            ResolutionExpectation {
                occurrence: unresolved_call,
                expected: ExpectedResolution::Unresolved,
            },
        ],
    )
    .expect("quality corpus is valid");

    assert_eq!(report.exact_precision.basis_points(), Some(10_000));
    assert_eq!(report.exact_recall.basis_points(), Some(10_000));
    assert_eq!(report.candidate_recall.basis_points(), Some(10_000));
    assert_eq!(report.ambiguous_hidden_exact, 0);
    assert_eq!(report.unresolved_correct, 1);

    let applied = engine
        .apply(
            fixture.document,
            ResolverFactContext::new(fixture.content_hash),
            &Cancellation::new(),
        )
        .expect("call fixture applies");
    assert_eq!(
        applied
            .document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::Calls)
            .count(),
        1
    );
}

struct Fixture {
    document: NormalizedIrDocument,
    primary_file: FileId,
    provenance: FactId,
    content_hash: ContentHash,
}

impl Fixture {
    fn new() -> Self {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let provenance = FactId::from_bytes([3; 20]);
        let primary_file = FileId::from_bytes([1; 20]);
        let content_hash = content_hash(&[0; 64]);
        let source = source_ref(
            repository,
            generation,
            primary_file,
            content_hash,
            0,
            SOURCE_BYTES,
        );
        let mut document = NormalizedIrDocument::empty(repository, generation);
        document.provenance.push(ProvenanceRecord {
            id: provenance,
            repository,
            generation,
            producer_kind: ProducerKind::Parser,
            producer: ProducerIdentity::new("rootlight-resolve-fixture", "1.0", content_hash)
                .expect("fixture producer is valid"),
            binary_digest: content_hash,
            frontend_version: Some("fixture-1".to_owned()),
            language: "rust".to_owned(),
            tier: AnalysisTier::TierB,
            build_context: BuildContextIdentity::new(content_hash),
            input_sources: vec![source.clone()],
            evidence_sources: vec![source],
            derivation_parents: Vec::new(),
            rule: None,
        });
        document.files.push(file_record(
            repository,
            generation,
            primary_file,
            provenance,
            content_hash,
            "src/lib.rs",
        ));
        Self {
            document,
            primary_file,
            provenance,
            content_hash,
        }
    }

    fn add_file(&mut self, identity: u8, path: &str) -> FileId {
        let file = FileId::from_bytes([identity; 20]);
        self.document.files.push(file_record(
            self.document.repository,
            self.document.generation,
            file,
            self.provenance,
            self.content_hash,
            path,
        ));
        file
    }

    fn add_entity(
        &mut self,
        identity: u8,
        name: &str,
        file: FileId,
        kind: EntityKind,
        enclosing: Option<SymbolId>,
    ) -> SymbolId {
        let symbol = SymbolId::from_bytes([identity; 20]);
        let source = source_ref(
            self.document.repository,
            self.document.generation,
            file,
            self.content_hash,
            0,
            8,
        );
        self.document.entities.push(EntityRecord {
            id: symbol,
            repository: self.document.repository,
            generation: self.document.generation,
            kind,
            language: "rust".to_owned(),
            tier: AnalysisTier::TierB,
            canonical_name: name.to_owned(),
            display_name: name.to_owned(),
            qualified_name: format!("fixture::{name}"),
            container: enclosing
                .map(ContainerRef::Entity)
                .or(Some(ContainerRef::File(file))),
            visibility: EntityVisibility::Private,
            flags: Vec::<EntityFlag>::new(),
            provenance: self.provenance,
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        });
        symbol
    }

    fn add_occurrence(
        &mut self,
        identity: u8,
        spelling: &str,
        file: FileId,
        role: OccurrenceRole,
        enclosing: Option<SymbolId>,
    ) -> FactId {
        let occurrence = FactId::from_bytes([identity; 20]);
        let source = source_ref(
            self.document.repository,
            self.document.generation,
            file,
            self.content_hash,
            16,
            24,
        );
        let text_hash = content_hash(spelling.as_bytes());
        self.document.occurrences.push(OccurrenceRecord {
            id: occurrence,
            repository: self.document.repository,
            generation: self.document.generation,
            file,
            source: source.clone(),
            role,
            enclosing,
            target: OccurrenceTarget::Unresolved { text_hash },
            syntactic_text_hash: text_hash,
            syntax_kind: "identifier".to_owned(),
            provenance: self.provenance,
            confidence: Confidence::new(0).expect("zero confidence is valid"),
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        });
        occurrence
    }

    fn validate(&self) {
        validate_ir_document(
            &self.document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("resolution fixture is valid");
    }
}

fn file_record(
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    provenance: FactId,
    content_hash: ContentHash,
    path: &str,
) -> FileRecord {
    let source = source_ref(repository, generation, file, content_hash, 0, SOURCE_BYTES);
    FileRecord {
        id: file,
        repository,
        generation,
        path: path.to_owned(),
        path_locator: None,
        content_hash,
        byte_length: SOURCE_BYTES,
        language: "rust".to_owned(),
        encoding: "utf-8".to_owned(),
        generated: false,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: Vec::new(),
        },
    }
}

fn source_ref(
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    content_hash: ContentHash,
    start: u64,
    end: u64,
) -> SourceRef {
    SourceRef::new(
        repository,
        generation,
        SourceSpan::new(file, start, end).expect("fixture span is ordered"),
        content_hash,
        None,
    )
}
