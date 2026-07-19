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
    EntityKind, EntityRecord, EntityVisibility, ExtensionSupport, FactEvidence, FileRecord,
    IrLimits, NormalizedIrDocument, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    ProducerIdentity, ProducerKind, ProvenanceRecord, SourceRef, SourceSpan, validate_ir_document,
};
use rootlight_resolve::{
    CompletenessAssumption, RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION, ResolutionError,
    ResolutionSignal, UnresolvedReason,
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
