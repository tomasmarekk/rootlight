//! Source-owned Haskell equation and annotation lowering contracts.
//!
//! Authored syntax providers isolate grouping and exact formal-identity barriers;
//! native admission is covered separately. No types or call dispatch are inferred.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds, CoverageReport, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, MemoryEnforcement,
    ParseCapabilities, StreamLimits, SyntaxFact, SyntaxFactKind, SyntaxKindLabel, execute_analysis,
    testkit::MockParseProvider,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, CoverageStatus, EntityKind, ExtensionSupport,
    FactRef, IrDocument, IrLimits, LEXICAL_EXTENSION_NAMESPACE, OccurrenceRole, OccurrenceTarget,
    ProducerIdentity, RelationEndpoint, RelationPredicate, SourceRef, SourceSpan,
    decode_ir_document, decode_lexical_evidence_envelope,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

struct Fixture {
    _directory: TempDir,
    text: String,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(text: &str) -> Self {
        let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::write(directory.path().join("Sample.hs"), text).unwrap();
        let repository = derive_repository(b"haskell-lowering-source-contract").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new("Sample.hs")).unwrap(), 4096)
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([7; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        Self {
            _directory: directory,
            text: text.to_owned(),
            snapshot,
            source,
        }
    }

    fn span(&self, needle: &str, index: usize) -> SourceSpan {
        let start = self.text.match_indices(needle).nth(index).unwrap().0;
        SourceSpan::new(
            self.source.span().file(),
            u64::try_from(start).unwrap(),
            u64::try_from(start + needle.len()).unwrap(),
        )
        .unwrap()
    }

    fn fact(
        &self,
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        selection: (&str, usize),
        depth: usize,
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            self.span(selection.0, selection.1),
            depth,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn module(&self) -> Vec<SyntaxFact> {
        vec![
            SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Root,
                self.source.span(),
                0,
                SyntaxKindLabel::new("haskell.file.root").unwrap(),
            ),
            SyntaxFact::new(
                2,
                Some(1),
                SyntaxFactKind::Module,
                self.source.span(),
                1,
                SyntaxKindLabel::new("haskell.module.module").unwrap(),
            ),
            self.fact(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                ("Sample", 0),
                2,
                "haskell.binding_name.definition",
            ),
            SyntaxFact::new(
                4,
                Some(2),
                SyntaxFactKind::Scope,
                self.source.span(),
                2,
                SyntaxKindLabel::new("haskell.file.scope").unwrap(),
            ),
        ]
    }

    fn analyze(
        &self,
        facts: Vec<SyntaxFact>,
        ir: IrLimits,
        status: CoverageStatus,
        cancellation: &Cancellation,
    ) -> Result<rootlight_adapter_sdk::AnalysisOutput, AdapterError> {
        let language = LanguageId::new("haskell").unwrap();
        let capabilities = ParseCapabilities::new(
            vec![language.clone()],
            vec![EncodingId::utf8()],
            4096,
            4096,
            64,
            8,
            true,
            true,
            true,
            4,
            MemoryEnforcement::AccountedInProcess,
        )
        .unwrap();
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            status,
            self.text.len(),
            self.text.len(),
            0,
            Vec::new(),
        )
        .unwrap();
        let provider = MockParseProvider::new(capabilities, facts, Vec::new(), coverage);
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(provider),
            ProducerIdentity::new(
                "haskell-lowering-contract",
                "1.0",
                content_hash(b"haskell-lowering-configuration"),
            )
            .unwrap(),
            language.clone(),
            "authored-syntax-provider",
            content_hash(b"authored-provider"),
        )
        .unwrap();
        let batch = BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap();
        let stream = StreamLimits::new(
            128,
            1024,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            batch,
        )
        .unwrap();
        let limits = AnalysisLimits::new(
            4096,
            4096,
            64,
            8,
            16 * 1024 * 1024,
            stream.clone(),
            stream,
            ir,
        )
        .unwrap();
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            language,
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"haskell-build-context")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancellation,
        )?;
        for occurrence in &output.document().occurrences {
            assert_eq!(
                occurrence.id,
                rootlight_ir::derive_occurrence_record_id(occurrence).unwrap(),
                "occurrence identity must include its final evidence"
            );
        }
        Ok(output)
    }
}

fn equation_facts(fixture: &Fixture) -> Vec<SyntaxFact> {
    let mut facts = fixture.module();
    facts.extend([
        fixture.fact(
            5,
            Some(4),
            SyntaxFactKind::Signature,
            ("pick :: Int -> Int", 0),
            3,
            "haskell.written_signature.signature",
        ),
        fixture.fact(
            6,
            Some(4),
            SyntaxFactKind::Declaration,
            ("pick", 0),
            3,
            "haskell.signature_name.declaration",
        ),
        fixture.fact(
            7,
            Some(6),
            SyntaxFactKind::Occurrence,
            ("pick", 0),
            4,
            "haskell.binding_name.definition",
        ),
    ]);
    let first = if fixture.text.contains("pick 0 = 9") {
        "pick 0 = 9"
    } else {
        "pick 0 = 1"
    };
    for (base, declaration, header, occurrence) in [
        (10, first, "pick 0", 1),
        (20, "pick value = value", "pick value", 2),
        (30, "pick value = 2", "pick value", 3),
    ] {
        facts.extend([
            fixture.fact(
                base,
                Some(4),
                SyntaxFactKind::Declaration,
                (declaration, 0),
                3,
                "haskell.function.declaration",
            ),
            fixture.fact(
                base + 1,
                Some(base),
                SyntaxFactKind::Occurrence,
                ("pick", occurrence),
                4,
                "haskell.binding_name.definition",
            ),
            fixture.fact(
                base + 2,
                Some(base),
                SyntaxFactKind::Signature,
                (header, usize::from(base == 30)),
                4,
                "haskell.function_header.signature",
            ),
            fixture.fact(
                base + 3,
                Some(base),
                SyntaxFactKind::Scope,
                (declaration, 0),
                4,
                "haskell.function_clause.scope",
            ),
        ]);
        if base != 10 {
            let value_index = if base == 20 { 0 } else { 2 };
            facts.extend([
                fixture.fact(
                    base + 4,
                    Some(base + 3),
                    SyntaxFactKind::Declaration,
                    ("value", value_index),
                    5,
                    "haskell.parameter.declaration",
                ),
                fixture.fact(
                    base + 5,
                    Some(base + 4),
                    SyntaxFactKind::Occurrence,
                    ("value", value_index),
                    6,
                    "haskell.binding_name.definition",
                ),
            ]);
        }
    }
    facts.push(fixture.fact(
        40,
        Some(23),
        SyntaxFactKind::Occurrence,
        ("value", 1),
        5,
        "haskell.identifier.reference",
    ));
    facts
}

#[test]
fn repeated_equations_share_one_entity_and_keep_every_source_owned_witness() {
    let fixture = Fixture::new(
        "module Sample where\npick :: Int -> Int\npick 0 = 1\npick value = value\npick value = 2\n",
    );
    let output = fixture
        .analyze(
            equation_facts(&fixture),
            IrLimits::default(),
            CoverageStatus::Complete,
            &deadline(),
        )
        .unwrap();
    let document = output.document();
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Module)
            .count(),
        1
    );
    let functions: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .collect();
    assert_eq!(functions.len(), 1);
    let function = functions[0];
    assert_eq!(function.canonical_name, "pick");
    assert_eq!(
        function.evidence.source.as_ref().unwrap().span(),
        fixture.span("pick 0 = 1", 0)
    );
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target
                    == OccurrenceTarget::Resolved {
                        symbol: function.id,
                    }
        })
        .collect();
    assert_eq!(definitions.len(), 3);
    for (definition, declaration) in ["pick 0 = 1", "pick value = value", "pick value = 2"]
        .into_iter()
        .enumerate()
    {
        let source = fixture.span("pick", definition + 1);
        let witness = definitions
            .iter()
            .find(|occurrence| occurrence.source.span() == source)
            .unwrap();
        assert_eq!(
            witness.evidence.source.as_ref().unwrap().span(),
            fixture.span(declaration, 0)
        );
        assert!(document.relations.iter().any(|relation| relation.predicate
            == RelationPredicate::Contains
            && relation.object == RelationEndpoint::Entity(function.id)
            && relation.evidence.source.as_ref().unwrap().span() == fixture.span(declaration, 0)));
    }
    let annotation = document
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::Declaration
                && occurrence.source.span() == fixture.span("pick", 0)
        })
        .unwrap();
    assert_eq!(
        annotation.target,
        OccurrenceTarget::Resolved {
            symbol: function.id
        }
    );
    assert_eq!(
        annotation.evidence.source.as_ref().unwrap().span(),
        fixture.span("pick :: Int -> Int", 0)
    );
    let signatures: Vec<_> = document
        .extensions
        .iter()
        .filter(|extension| extension.namespace == LEXICAL_EXTENSION_NAMESPACE)
        .map(|extension| decode_lexical_evidence_envelope(extension).unwrap())
        .collect();
    assert_eq!(signatures.len(), 4);
    assert!(
        signatures
            .iter()
            .any(|signature| signature.subject() == FactRef::Fact(annotation.id))
    );
    assert_eq!(
        signatures
            .iter()
            .filter(|signature| signature.subject() == FactRef::Entity(function.id))
            .count(),
        3
    );
    let parameters: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter)
        .collect();
    assert_eq!(
        parameters.len(),
        2,
        "same-name formals belong to different source clauses"
    );
    assert_ne!(parameters[0].id, parameters[1].id);
    assert!(
        parameters
            .iter()
            .all(|parameter| parameter.container == Some(ContainerRef::Entity(function.id)))
    );
    let read = document
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .unwrap();
    assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "haskell-binding-target-unavailable"
                && gap.source.span() == read.source.span())
    );
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    let encoded = serde_json::to_vec(document).unwrap();
    assert_eq!(
        decode_ir_document(&encoded, &IrLimits::default(), &ExtensionSupport::default()).unwrap(),
        IrDocument::NormalizedV1_1(document.clone())
    );
}

#[test]
fn equation_patterns_and_bodies_do_not_create_signature_overloads() {
    let texts = [
        "module Sample where\npick :: Int -> Int\npick 0 = 1\npick value = value\npick value = 2\n",
        "module Sample where\npick :: Int -> Int\npick 0 = 9\npick value = value\npick value = 2\n",
    ];
    let mut symbols = Vec::new();
    for text in texts {
        let fixture = Fixture::new(text);
        let output = fixture
            .analyze(
                equation_facts(&fixture),
                IrLimits::default(),
                CoverageStatus::Complete,
                &deadline(),
            )
            .unwrap();
        symbols.push(
            output
                .document()
                .entities
                .iter()
                .find(|entity| entity.kind == EntityKind::Function)
                .unwrap()
                .id,
        );
    }
    assert_eq!(symbols[0], symbols[1]);
}

#[test]
fn multi_name_annotations_do_not_invent_callable_entities_or_poison_the_module() {
    let fixture = Fixture::new("module Sample where\nleft, right :: Int\nleft = 1\n");
    let mut facts = fixture.module();
    facts.push(fixture.fact(
        5,
        Some(4),
        SyntaxFactKind::Signature,
        ("left, right :: Int", 0),
        3,
        "haskell.written_signature.signature",
    ));
    for (id, name) in [(6, "left"), (8, "right")] {
        facts.extend([
            fixture.fact(
                id,
                Some(4),
                SyntaxFactKind::Declaration,
                (name, 0),
                3,
                "haskell.signature_name.declaration",
            ),
            fixture.fact(
                id + 1,
                Some(id),
                SyntaxFactKind::Occurrence,
                (name, 0),
                4,
                "haskell.binding_name.definition",
            ),
        ]);
    }
    facts.extend([
        fixture.fact(
            10,
            Some(4),
            SyntaxFactKind::Declaration,
            ("left = 1", 0),
            3,
            "haskell.variable.declaration",
        ),
        fixture.fact(
            11,
            Some(10),
            SyntaxFactKind::Occurrence,
            ("left", 1),
            4,
            "haskell.binding_name.definition",
        ),
        fixture.fact(
            12,
            Some(10),
            SyntaxFactKind::Signature,
            ("left", 1),
            4,
            "haskell.binding_header.signature",
        ),
    ]);
    let output = fixture
        .analyze(
            facts,
            IrLimits::default(),
            CoverageStatus::Complete,
            &deadline(),
        )
        .unwrap();
    let document = output.document();
    assert_eq!(document.entities.len(), 2);
    assert!(
        document
            .entities
            .iter()
            .all(|entity| matches!(entity.kind, EntityKind::Module | EntityKind::Variable))
    );
    let variable = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable)
        .unwrap();
    let left = document
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::Declaration
                && occurrence.source.span() == fixture.span("left", 0)
        })
        .unwrap();
    let right = document
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::Declaration
                && occurrence.source.span() == fixture.span("right", 0)
        })
        .unwrap();
    assert_eq!(
        left.target,
        OccurrenceTarget::Resolved {
            symbol: variable.id
        }
    );
    assert!(matches!(right.target, OccurrenceTarget::Unresolved { .. }));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "haskell-annotation-owner-unavailable"
                && gap.source.span() == right.source.span())
    );
    assert_eq!(
        document
            .extensions
            .iter()
            .filter(|extension| extension.namespace == LEXICAL_EXTENSION_NAMESPACE)
            .count(),
        3
    );
}

#[test]
fn annotation_names_in_disjoint_local_groups_never_cross_attach() {
    let fixture = Fixture::new(
        "module Sample where\nleft = let pick :: Int; pick = 1 in pick\nright = let pick :: Int; pick = 2 in pick\n",
    );
    let mut facts = fixture.module();
    for (base, owner, body, group, value, offset) in [
        (
            10,
            "left",
            "left = let pick :: Int; pick = 1 in pick",
            "pick :: Int; pick = 1",
            "pick = 1",
            0,
        ),
        (
            30,
            "right",
            "right = let pick :: Int; pick = 2 in pick",
            "pick :: Int; pick = 2",
            "pick = 2",
            1,
        ),
    ] {
        facts.extend([
            fixture.fact(
                base,
                Some(4),
                SyntaxFactKind::Declaration,
                (body, 0),
                3,
                "haskell.variable.declaration",
            ),
            fixture.fact(
                base + 1,
                Some(base),
                SyntaxFactKind::Occurrence,
                (owner, 0),
                4,
                "haskell.binding_name.definition",
            ),
            fixture.fact(
                base + 2,
                Some(base),
                SyntaxFactKind::Signature,
                (owner, 0),
                4,
                "haskell.binding_header.signature",
            ),
            fixture.fact(
                base + 3,
                Some(base),
                SyntaxFactKind::Scope,
                (group, 0),
                4,
                "haskell.let_binds.scope",
            ),
            fixture.fact(
                base + 4,
                Some(base + 3),
                SyntaxFactKind::Signature,
                ("pick :: Int", offset),
                5,
                "haskell.written_signature.signature",
            ),
            fixture.fact(
                base + 5,
                Some(base + 3),
                SyntaxFactKind::Declaration,
                ("pick", offset * 3),
                5,
                "haskell.signature_name.declaration",
            ),
            fixture.fact(
                base + 6,
                Some(base + 5),
                SyntaxFactKind::Occurrence,
                ("pick", offset * 3),
                6,
                "haskell.binding_name.definition",
            ),
            fixture.fact(
                base + 7,
                Some(base + 3),
                SyntaxFactKind::Declaration,
                (value, 0),
                5,
                "haskell.variable.declaration",
            ),
            fixture.fact(
                base + 8,
                Some(base + 7),
                SyntaxFactKind::Occurrence,
                ("pick", offset * 3 + 1),
                6,
                "haskell.binding_name.definition",
            ),
            fixture.fact(
                base + 9,
                Some(base + 7),
                SyntaxFactKind::Signature,
                ("pick", offset * 3 + 1),
                6,
                "haskell.binding_header.signature",
            ),
        ]);
    }
    let output = fixture
        .analyze(
            facts,
            IrLimits::default(),
            CoverageStatus::Complete,
            &deadline(),
        )
        .unwrap();
    let document = output.document();
    let picks: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "pick")
        .collect();
    assert_eq!(picks.len(), 2);
    assert_ne!(picks[0].id, picks[1].id);
    for annotation in document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Declaration)
    {
        let OccurrenceTarget::Resolved { symbol } = annotation.target else {
            panic!("source annotation attaches within its group")
        };
        let target = picks.iter().find(|entity| entity.id == symbol).unwrap();
        assert_eq!(
            annotation.enclosing,
            target.container.and_then(|container| match container {
                ContainerRef::Entity(parent) => Some(parent),
                _ => None,
            })
        );
    }
}

#[test]
fn incomplete_capture_plans_do_not_claim_annotation_binding_and_limits_stay_hard() {
    let fixture = Fixture::new(
        "module Sample where\npick :: Int -> Int\npick 0 = 1\npick value = value\npick value = 2\n",
    );
    let output = fixture
        .analyze(
            equation_facts(&fixture),
            IrLimits::default(),
            CoverageStatus::Bounded,
            &deadline(),
        )
        .unwrap();
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::Declaration)
            .all(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
    let mut limited_ir = IrLimits::default();
    limited_ir.max_occurrences = 1;
    assert!(
        fixture
            .analyze(
                equation_facts(&fixture),
                limited_ir,
                CoverageStatus::Complete,
                &deadline()
            )
            .is_err()
    );
    let cancelled = deadline();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        fixture.analyze(
            equation_facts(&fixture),
            IrLimits::default(),
            CoverageStatus::Complete,
            &cancelled
        ),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn exact_native_clause_labels_resolve_only_with_complete_parse_and_available_identity() {
    let fixture = Fixture::new(
        "module Sample where\npick :: Int -> Int\npick 0 = 1\npick value = value\npick value = 2\n",
    );
    for (status, missing_definition, marker_parent, resolved) in [
        (CoverageStatus::Complete, false, 20, true),
        (CoverageStatus::Complete, false, 23, true),
        (CoverageStatus::Bounded, false, 20, false),
        (CoverageStatus::Complete, true, 20, false),
    ] {
        let mut facts = equation_facts(&fixture);
        facts
            .retain(|fact| fact.local_id() != 40 && !(missing_definition && fact.local_id() == 25));
        facts.extend([
            fixture.fact(
                41,
                Some(marker_parent),
                SyntaxFactKind::Signature,
                ("= value", 0),
                if marker_parent == 23 { 5 } else { 4 },
                "haskell.simple_equation_match.signature",
            ),
            fixture.fact(
                40,
                Some(23),
                SyntaxFactKind::Occurrence,
                ("value", 1),
                5,
                "haskell.equation_rhs_read.reference",
            ),
        ]);
        let output = fixture
            .analyze(facts.clone(), IrLimits::default(), status, &deadline())
            .unwrap();
        let document = output.document();
        let read = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span() == fixture.span("value", 1)
            })
            .unwrap();
        assert_eq!(
            matches!(read.target, OccurrenceTarget::Resolved { .. }),
            resolved
        );
        let relations = document
            .relations
            .iter()
            .filter(|relation| relation.subject == RelationEndpoint::Occurrence(read.id))
            .collect::<Vec<_>>();
        assert_eq!(relations.len(), usize::from(resolved));
        if let OccurrenceTarget::Resolved { symbol } = &read.target {
            let parameter = document
                .entities
                .iter()
                .find(|entity| entity.id == *symbol)
                .unwrap();
            assert_eq!(parameter.kind, EntityKind::Parameter);
            assert_eq!(parameter.canonical_name, "value");
            assert_eq!(relations[0].predicate, RelationPredicate::RefersTo);
            assert_eq!(relations[0].object, RelationEndpoint::Entity(*symbol));
            assert_eq!(relations[0].evidence.source.as_ref(), Some(&read.source));
        }
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "haskell-binding-target-unavailable"
                    && gap.source.span() == read.source.span()),
            !resolved
        );
        assert!(
            document
                .relations
                .iter()
                .all(|relation| relation.predicate != RelationPredicate::Calls)
        );
        if resolved && marker_parent == 20 {
            for resource in 0..3 {
                let mut limits = IrLimits::default();
                match resource {
                    0 => limits.max_entities = 0,
                    1 => limits.max_occurrences = 0,
                    2 => limits.max_relations = 0,
                    _ => unreachable!(),
                }
                assert!(matches!(
                    fixture.analyze(facts.clone(), limits, status, &deadline()),
                    Err(AdapterError::Sink(
                        rootlight_adapter_sdk::SinkError::StreamLimit { .. }
                    ))
                ));
            }
            let cancelled = deadline();
            cancelled.cancel(CancellationReason::ClientRequest);
            assert!(matches!(
                fixture.analyze(facts, IrLimits::default(), status, &cancelled),
                Err(AdapterError::Cancelled { .. })
            ));
        }
    }
}

#[test]
fn calls_types_and_imports_keep_exact_unresolved_sources_even_with_matching_entities() {
    let fixture = Fixture::new(
        "module Sample where\nimport Peer\nf :: Number -> Number\nf x = x\nuse = f 1\n",
    );
    let mut facts = fixture.module();
    facts.extend([
        fixture.fact(
            5,
            Some(4),
            SyntaxFactKind::Import,
            ("import Peer", 0),
            3,
            "haskell.import.import",
        ),
        fixture.fact(
            6,
            Some(4),
            SyntaxFactKind::Signature,
            ("f :: Number -> Number", 0),
            3,
            "haskell.written_signature.signature",
        ),
        fixture.fact(
            7,
            Some(4),
            SyntaxFactKind::Declaration,
            ("f", 0),
            3,
            "haskell.signature_name.declaration",
        ),
        fixture.fact(
            8,
            Some(7),
            SyntaxFactKind::Occurrence,
            ("f", 0),
            4,
            "haskell.binding_name.definition",
        ),
        fixture.fact(
            9,
            Some(4),
            SyntaxFactKind::Occurrence,
            ("Number", 0),
            3,
            "haskell.type_name.reference",
        ),
        fixture.fact(
            10,
            Some(4),
            SyntaxFactKind::Occurrence,
            ("Number", 1),
            3,
            "haskell.type_name.reference",
        ),
        fixture.fact(
            11,
            Some(4),
            SyntaxFactKind::Declaration,
            ("f x = x", 0),
            3,
            "haskell.function.declaration",
        ),
        fixture.fact(
            12,
            Some(11),
            SyntaxFactKind::Occurrence,
            ("f", 1),
            4,
            "haskell.binding_name.definition",
        ),
        fixture.fact(
            13,
            Some(11),
            SyntaxFactKind::Signature,
            ("f x", 0),
            4,
            "haskell.function_header.signature",
        ),
        fixture.fact(
            14,
            Some(11),
            SyntaxFactKind::Scope,
            ("f x = x", 0),
            4,
            "haskell.function_clause.scope",
        ),
        fixture.fact(
            15,
            Some(14),
            SyntaxFactKind::Declaration,
            ("x", 0),
            5,
            "haskell.parameter.declaration",
        ),
        fixture.fact(
            16,
            Some(15),
            SyntaxFactKind::Occurrence,
            ("x", 0),
            6,
            "haskell.binding_name.definition",
        ),
        fixture.fact(
            17,
            Some(14),
            SyntaxFactKind::Occurrence,
            ("x", 1),
            5,
            "haskell.identifier.reference",
        ),
        fixture.fact(
            20,
            Some(4),
            SyntaxFactKind::Declaration,
            ("use = f 1", 0),
            3,
            "haskell.variable.declaration",
        ),
        fixture.fact(
            21,
            Some(20),
            SyntaxFactKind::Occurrence,
            ("use", 0),
            4,
            "haskell.binding_name.definition",
        ),
        fixture.fact(
            22,
            Some(20),
            SyntaxFactKind::Signature,
            ("use", 0),
            4,
            "haskell.binding_header.signature",
        ),
        fixture.fact(
            23,
            Some(20),
            SyntaxFactKind::Scope,
            ("use = f 1", 0),
            4,
            "haskell.variable_binding.scope",
        ),
        fixture.fact(
            24,
            Some(23),
            SyntaxFactKind::Occurrence,
            ("f 1", 0),
            5,
            "haskell.application.call",
        ),
        fixture.fact(
            25,
            Some(24),
            SyntaxFactKind::Occurrence,
            ("f", 2),
            6,
            "haskell.application_name.call_name",
        ),
    ]);
    let output = fixture
        .analyze(
            facts,
            IrLimits::default(),
            CoverageStatus::Complete,
            &deadline(),
        )
        .unwrap();
    let document = output.document();
    let uses: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            !matches!(
                occurrence.role,
                OccurrenceRole::Definition | OccurrenceRole::Declaration
            )
        })
        .collect();
    assert_eq!(uses.len(), 5);
    assert!(
        uses.iter()
            .all(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert_eq!(
        uses.iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::TypeUse)
            .count(),
        2
    );
    assert_eq!(
        uses.iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .count(),
        1
    );
    assert_eq!(
        uses.iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::ImportUse)
            .count(),
        1
    );
    for occurrence in uses {
        assert!(document.skipped_regions.iter().any(|gap| gap.source.span()
            == occurrence.source.span()
            && matches!(
                gap.detail.as_str(),
                "haskell-binding-target-unavailable" | "unresolved-import-target"
            )));
    }
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
}

#[test]
fn overlapping_annotation_evidence_is_not_attached_by_nearest_spelling() {
    let fixture = Fixture::new(
        "module Sample where\npick :: Int -> Int\npick 0 = 1\npick value = value\npick value = 2\n",
    );
    let mut facts = equation_facts(&fixture);
    facts.push(fixture.fact(
        50,
        Some(4),
        SyntaxFactKind::Signature,
        ("pick :: Int", 0),
        3,
        "haskell.written_signature.signature",
    ));
    let output = fixture
        .analyze(
            facts,
            IrLimits::default(),
            CoverageStatus::Complete,
            &deadline(),
        )
        .unwrap();
    let document = output.document();
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        1
    );
    let annotation = document
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::Declaration)
        .unwrap();
    assert!(matches!(
        annotation.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(annotation.source.span(), fixture.span("pick", 0));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "haskell-annotation-owner-unavailable")
    );
    assert_eq!(
        document
            .extensions
            .iter()
            .filter(|extension| extension.namespace == LEXICAL_EXTENSION_NAMESPACE)
            .count(),
        3
    );
}

#[test]
fn orphan_or_missing_name_annotations_report_exact_gaps_without_synthesizing_bindings() {
    let fixture = Fixture::new("module Sample where\nalone :: Int\n");
    for include_head in [false, true] {
        let mut facts = fixture.module();
        facts.push(fixture.fact(
            5,
            Some(4),
            SyntaxFactKind::Signature,
            ("alone :: Int", 0),
            3,
            "haskell.written_signature.signature",
        ));
        if include_head {
            facts.push(fixture.fact(
                6,
                Some(4),
                SyntaxFactKind::Declaration,
                ("alone", 0),
                3,
                "haskell.signature_name.declaration",
            ));
        }
        let output = fixture
            .analyze(
                facts,
                IrLimits::default(),
                CoverageStatus::Complete,
                &deadline(),
            )
            .unwrap();
        let document = output.document();
        assert_eq!(document.entities.len(), 1);
        assert_eq!(document.entities[0].kind, EntityKind::Module);
        assert_eq!(document.occurrences.len(), 1);
        assert_eq!(document.occurrences[0].role, OccurrenceRole::Definition);
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.detail == "haskell-annotation-owner-unavailable"
                && gap.source.span() == fixture.span("alone :: Int", 0)
        }));
        if include_head {
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.detail == "haskell-annotation-owner-unavailable"
                    && gap.source.span() == fixture.span("alone", 0)
            }));
        }
    }
}

fn class_member_facts(fixture: &Fixture) -> Vec<SyntaxFact> {
    let mut facts = fixture.module();
    let class = fixture
        .text
        .strip_prefix("module Sample where\n")
        .unwrap()
        .trim_end();
    facts.extend([
        fixture.fact(
            5,
            Some(4),
            SyntaxFactKind::Declaration,
            (class, 0),
            3,
            "haskell.class.declaration",
        ),
        fixture.fact(
            6,
            Some(5),
            SyntaxFactKind::Occurrence,
            ("Select", 0),
            4,
            "haskell.binding_name.definition",
        ),
        fixture.fact(
            7,
            Some(5),
            SyntaxFactKind::Signature,
            ("class Select a", 0),
            4,
            "haskell.declaration_header.signature",
        ),
        fixture.fact(
            8,
            Some(5),
            SyntaxFactKind::Scope,
            (class, 0),
            4,
            "haskell.class.scope",
        ),
        fixture.fact(
            10,
            Some(8),
            SyntaxFactKind::Signature,
            ("pick :: a -> a", 0),
            5,
            "haskell.written_signature.signature",
        ),
        fixture.fact(
            11,
            Some(8),
            SyntaxFactKind::Declaration,
            ("pick", 0),
            5,
            "haskell.signature_member_name.declaration",
        ),
        fixture.fact(
            12,
            Some(11),
            SyntaxFactKind::Occurrence,
            ("pick", 0),
            6,
            "haskell.binding_name.definition",
        ),
    ]);
    facts
}

#[test]
fn complete_class_signature_member_publishes_exact_definition_and_annotation() {
    let fixture = Fixture::new("module Sample where\nclass Select a where\n  pick :: a -> a\n");
    let full = fixture
        .analyze(
            class_member_facts(&fixture),
            IrLimits::default(),
            CoverageStatus::Complete,
            &deadline(),
        )
        .unwrap();
    let document = full.document();
    let method = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Method)
        .unwrap();
    let class = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Trait)
        .unwrap();
    assert_eq!(method.container, Some(ContainerRef::Entity(class.id)));
    assert_eq!(method.canonical_name, "pick");
    assert_eq!(method.qualified_name, "Sample::Select::pick");
    assert_eq!(
        method.evidence.source.as_ref().unwrap().span(),
        fixture.span("pick :: a -> a", 0)
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Parameter)
    );
    for role in [OccurrenceRole::Definition, OccurrenceRole::Declaration] {
        let witnesses: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.role == role && site.source.span() == fixture.span("pick", 0))
            .collect();
        assert_eq!(witnesses.len(), 1);
        assert_eq!(
            witnesses[0].target,
            OccurrenceTarget::Resolved { symbol: method.id }
        );
        assert_eq!(
            witnesses[0].source.generation(),
            fixture.source.generation()
        );
        assert_eq!(
            witnesses[0].source.content_hash(),
            fixture.source.content_hash()
        );
    }
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.predicate == RelationPredicate::Contains)
    );
    assert!(document.relations.iter().any(|edge| {
        edge.subject == RelationEndpoint::Entity(class.id)
            && edge.object == RelationEndpoint::Entity(method.id)
            && edge.evidence.source.as_ref().unwrap().span() == fixture.span("pick :: a -> a", 0)
    }));
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "haskell-annotation-owner-unavailable")
    );
    let encoded = serde_json::to_vec(document).unwrap();
    assert_eq!(
        decode_ir_document(&encoded, &IrLimits::default(), &ExtensionSupport::default()).unwrap(),
        IrDocument::NormalizedV1_1(document.clone())
    );
    let mut bounded = IrLimits::default();
    bounded.max_extensions = document.entities.len() + 1;
    let output = fixture
        .analyze(
            class_member_facts(&fixture),
            bounded,
            CoverageStatus::Complete,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Method)
            .unwrap()
            .id,
        method.id
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "lexical-evidence-resource-limit")
    );
}

#[test]
fn incomplete_or_ambiguous_class_member_evidence_never_creates_a_method() {
    let fixture = Fixture::new("module Sample where\nclass Select a where\n  pick :: a -> a\n");
    for (remove, overlap, status) in [
        (None, false, CoverageStatus::Bounded),
        (Some(12), false, CoverageStatus::Complete),
        (Some(10), false, CoverageStatus::Complete),
        (Some(6), false, CoverageStatus::Complete),
        (None, true, CoverageStatus::Complete),
    ] {
        let mut facts = class_member_facts(&fixture);
        facts.retain(|fact| Some(fact.local_id()) != remove);
        if overlap {
            facts.push(fixture.fact(
                13,
                Some(8),
                SyntaxFactKind::Signature,
                ("pick :: a", 0),
                5,
                "haskell.written_signature.signature",
            ));
        }
        let output = fixture
            .analyze(facts, IrLimits::default(), status, &deadline())
            .unwrap();
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Method),
            "remove={remove:?}, overlap={overlap}, status={status:?}"
        );
        assert!(
            output
                .document()
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Declaration)
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "haskell-annotation-owner-unavailable")
        );
    }
    let duplicate = Fixture::new(
        "module Sample where\nclass Select a where\n  pick :: a -> a\n  pick :: a -> a\n",
    );
    let mut facts = class_member_facts(&duplicate);
    facts.extend([
        duplicate.fact(
            20,
            Some(8),
            SyntaxFactKind::Signature,
            ("pick :: a -> a", 1),
            5,
            "haskell.written_signature.signature",
        ),
        duplicate.fact(
            21,
            Some(8),
            SyntaxFactKind::Declaration,
            ("pick", 1),
            5,
            "haskell.signature_member_name.declaration",
        ),
        duplicate.fact(
            22,
            Some(21),
            SyntaxFactKind::Occurrence,
            ("pick", 1),
            6,
            "haskell.binding_name.definition",
        ),
    ]);
    let output = duplicate
        .analyze(
            facts,
            IrLimits::default(),
            CoverageStatus::Complete,
            &deadline(),
        )
        .unwrap();
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Method)
    );
    assert_eq!(
        output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Declaration)
            .count(),
        2
    );
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Declaration)
            .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn occupied_ambiguous_class_binding_is_not_treated_as_a_missing_method() {
    let fixture = Fixture::new(
        "module Sample where\nclass Select a where\n  pick :: a -> a\n  pick value = value\n  pick = 1\n",
    );
    let mut facts = class_member_facts(&fixture);
    facts.extend([
        fixture.fact(
            20,
            Some(8),
            SyntaxFactKind::Declaration,
            ("pick value = value", 0),
            5,
            "haskell.function.declaration",
        ),
        fixture.fact(
            21,
            Some(20),
            SyntaxFactKind::Occurrence,
            ("pick", 1),
            6,
            "haskell.binding_name.definition",
        ),
        fixture.fact(
            22,
            Some(20),
            SyntaxFactKind::Signature,
            ("pick value", 0),
            6,
            "haskell.function_header.signature",
        ),
        fixture.fact(
            30,
            Some(8),
            SyntaxFactKind::Declaration,
            ("pick = 1", 0),
            5,
            "haskell.variable.declaration",
        ),
        fixture.fact(
            31,
            Some(30),
            SyntaxFactKind::Occurrence,
            ("pick", 2),
            6,
            "haskell.binding_name.definition",
        ),
        fixture.fact(
            32,
            Some(30),
            SyntaxFactKind::Signature,
            ("pick", 2),
            6,
            "haskell.binding_header.signature",
        ),
    ]);
    let output = fixture
        .analyze(
            facts,
            IrLimits::default(),
            CoverageStatus::Complete,
            &deadline(),
        )
        .unwrap();
    let document = output.document();
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Method)
            .count(),
        1
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable)
            .count(),
        1
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span() == fixture.span("pick", 0))
    );
    let annotation = document
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::Declaration)
        .unwrap();
    assert!(matches!(
        annotation.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "haskell-annotation-owner-unavailable")
    );
}

#[test]
fn class_member_publication_obeys_record_limits_and_typed_cancellation() {
    let fixture = Fixture::new("module Sample where\nclass Select a where\n  pick :: a -> a\n");
    for maximum in 0..3 {
        let mut limits = IrLimits::default();
        limits.max_entities = maximum;
        assert!(matches!(
            fixture.analyze(
                class_member_facts(&fixture),
                limits,
                CoverageStatus::Complete,
                &deadline()
            ),
            Err(AdapterError::Sink(
                rootlight_adapter_sdk::SinkError::StreamLimit { .. }
            ))
        ));
    }
    for maximum in 0..4 {
        let mut limits = IrLimits::default();
        limits.max_occurrences = maximum;
        assert!(matches!(
            fixture.analyze(
                class_member_facts(&fixture),
                limits,
                CoverageStatus::Complete,
                &deadline()
            ),
            Err(AdapterError::Sink(
                rootlight_adapter_sdk::SinkError::StreamLimit { .. }
            ))
        ));
    }
    let mut exact = IrLimits::default();
    exact.max_entities = 3;
    exact.max_occurrences = 4;
    let output = fixture
        .analyze(
            class_member_facts(&fixture),
            exact,
            CoverageStatus::Complete,
            &deadline(),
        )
        .unwrap();
    assert_eq!(output.document().entities.len(), 3);
    assert_eq!(output.document().occurrences.len(), 4);
    let cancelled = deadline();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        fixture.analyze(
            class_member_facts(&fixture),
            IrLimits::default(),
            CoverageStatus::Complete,
            &cancelled
        ),
        Err(AdapterError::Cancelled { .. })
    ));
}
