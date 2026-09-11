//! End-to-end fixtures for the daemon-independent first query slice.

use std::{collections::BTreeSet, fs, path::Path, time::Duration};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{
    FactId, FileIdentity, GenerationIdentity, RepositoryId, SymbolId, content_hash, derive_fact,
    derive_file, derive_generation, derive_repository,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, Confidence, CoverageStatus, DiagnosticRecord,
    DiagnosticSeverity, EvidenceKind, ExtensionSupport, FactEvidence, FileRecord, IrDocument,
    IrLimits, NormalizedIrDocument, ProducerIdentity, ProducerKind, ProvenanceRecord,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan, decode_ir_document,
};
use rootlight_query::{
    ExecutionCompletenessState, GenerationSet, LexicalProjectionBuilder, LocateMode, PlanKind,
    QueryBudget, QueryError, QueryResource, QueryResponse, QueryService, RelationDirection,
    RelationFamily, RepositoryDataTrust, TokenAccountingProfile, project_lexical_documents,
    project_lexical_documents_with_all_sources, project_lexical_documents_with_sources,
};
use rootlight_search::{
    BuildBudget, LexicalSearch, QueryViolation, SearchBudget, SearchError, SearchHit,
    SearchOutcome, SearchRequest,
};
use rootlight_source::{SourceBudget, SourceReadOptions, SourceService};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationContext, GenerationManifestRecipe,
    GenerationMetadata, GenerationSnapshot, IdentityVerifiedGeneration,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::tempdir_in;

#[derive(Clone)]
struct FakeSearch {
    generation: rootlight_ids::GenerationId,
    hits: Vec<SearchHit>,
}

impl LexicalSearch for FakeSearch {
    fn generation(&self) -> rootlight_ids::GenerationId {
        self.generation
    }

    fn search_with_stats(
        &self,
        request: &SearchRequest,
        budget: SearchBudget,
        cancellation: &Cancellation,
    ) -> Result<SearchOutcome, SearchError> {
        self.search_with_language_filter_and_stats(request, &[], budget, cancellation)
    }

    fn search_with_language_filter_and_stats(
        &self,
        request: &SearchRequest,
        languages: &[String],
        _budget: SearchBudget,
        cancellation: &Cancellation,
    ) -> Result<SearchOutcome, SearchError> {
        cancellation.check()?;
        let filtered: Vec<_> = self
            .hits
            .iter()
            .filter(|hit| languages.is_empty() || languages.binary_search(&hit.language).is_ok())
            .collect();
        let materialized_text_bytes = filtered.iter().try_fold(0_u64, |total, hit| {
            [
                hit.identifier.len(),
                hit.qualified_name.len(),
                hit.path.len(),
                hit.kind.len(),
                hit.language.len(),
                hit.tier.len(),
            ]
            .into_iter()
            .try_fold(total, |subtotal, length| {
                subtotal.checked_add(u64::try_from(length).ok()?)
            })
        });
        Ok(SearchOutcome {
            hits: filtered
                .iter()
                .skip(request.page_offset)
                .take(request.max_results)
                .map(|hit| (*hit).clone())
                .collect(),
            matched_candidates: u64::try_from(filtered.len())
                .map_err(|_| SearchError::CandidateBudgetExceeded)?,
            materialized_text_bytes: materialized_text_bytes
                .ok_or(SearchError::ReturnedTextBudgetExceeded)?,
        })
    }

    fn document_count(&self) -> u64 {
        u64::try_from(self.hits.len()).expect("test hit count fits u64")
    }
}

struct UnderreportedSearch(FakeSearch);

impl LexicalSearch for UnderreportedSearch {
    fn generation(&self) -> rootlight_ids::GenerationId {
        self.0.generation
    }

    fn search_with_stats(
        &self,
        request: &SearchRequest,
        budget: SearchBudget,
        cancellation: &Cancellation,
    ) -> Result<SearchOutcome, SearchError> {
        let mut outcome = self.0.search_with_stats(request, budget, cancellation)?;
        outcome.materialized_text_bytes = 0;
        Ok(outcome)
    }

    fn search_with_language_filter_and_stats(
        &self,
        request: &SearchRequest,
        languages: &[String],
        budget: SearchBudget,
        cancellation: &Cancellation,
    ) -> Result<SearchOutcome, SearchError> {
        let mut outcome = self.0.search_with_language_filter_and_stats(
            request,
            languages,
            budget,
            cancellation,
        )?;
        outcome.materialized_text_bytes = 0;
        Ok(outcome)
    }

    fn document_count(&self) -> u64 {
        self.0.document_count()
    }
}

struct TruncatedSearch(FakeSearch);

impl LexicalSearch for TruncatedSearch {
    fn generation(&self) -> rootlight_ids::GenerationId {
        self.0.generation
    }

    fn search_with_stats(
        &self,
        request: &SearchRequest,
        budget: SearchBudget,
        cancellation: &Cancellation,
    ) -> Result<SearchOutcome, SearchError> {
        let mut outcome = self.0.search_with_stats(request, budget, cancellation)?;
        outcome.matched_candidates = outcome
            .matched_candidates
            .checked_add(1)
            .ok_or(SearchError::CandidateBudgetExceeded)?;
        Ok(outcome)
    }

    fn search_with_language_filter_and_stats(
        &self,
        request: &SearchRequest,
        languages: &[String],
        budget: SearchBudget,
        cancellation: &Cancellation,
    ) -> Result<SearchOutcome, SearchError> {
        let mut outcome = self.0.search_with_language_filter_and_stats(
            request,
            languages,
            budget,
            cancellation,
        )?;
        outcome.matched_candidates = outcome
            .matched_candidates
            .checked_add(1)
            .ok_or(SearchError::CandidateBudgetExceeded)?;
        Ok(outcome)
    }

    fn document_count(&self) -> u64 {
        self.0.document_count().saturating_add(1)
    }
}

struct BackendCancelledSearch(FakeSearch);

impl LexicalSearch for BackendCancelledSearch {
    fn generation(&self) -> rootlight_ids::GenerationId {
        self.0.generation
    }

    fn search_with_stats(
        &self,
        _request: &SearchRequest,
        _budget: SearchBudget,
        _cancellation: &Cancellation,
    ) -> Result<SearchOutcome, SearchError> {
        Err(SearchError::Cancelled(CancellationReason::ClientRequest))
    }

    fn search_with_language_filter_and_stats(
        &self,
        _request: &SearchRequest,
        _languages: &[String],
        _budget: SearchBudget,
        _cancellation: &Cancellation,
    ) -> Result<SearchOutcome, SearchError> {
        Err(SearchError::Cancelled(CancellationReason::ClientRequest))
    }

    fn document_count(&self) -> u64 {
        self.0.document_count()
    }
}

fn assert_exact_response_accounting<T>(response: &QueryResponse<T>)
where
    T: serde::Serialize,
{
    let exact_bytes = u64::try_from(
        serde_json::to_vec(response)
            .expect("response serializes")
            .len(),
    )
    .expect("response length fits");
    assert_eq!(response.usage.json_bytes, exact_bytes);
    assert_eq!(response.usage.estimated_tokens, exact_bytes);
    assert_eq!(
        response.usage.token_accounting,
        TokenAccountingProfile::Utf8ByteUpperBoundV1
    );
}

fn fixture_snapshot() -> GenerationSnapshot {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compatibility/ir/1.1/document.json");
    let encoded = fs::read(path).expect("compatibility fixture is readable");
    let IrDocument::NormalizedV1_1(template) =
        decode_ir_document(&encoded, &IrLimits::default(), &ExtensionSupport::default())
            .expect("compatibility fixture decodes")
    else {
        panic!("fixture uses normalized IR 1.1");
    };
    let manifest_hash = content_hash(b"query-fixture-manifest");
    let configuration_hash = content_hash(b"query-fixture-configuration");
    let provider_set_hash = content_hash(b"query-fixture-providers");
    let generation = derive_generation(GenerationIdentity {
        repository: template.repository,
        parent: None,
        manifest_hash,
        config_hash: configuration_hash,
        provider_set_hash,
        format_version: generation_format_version(),
    })
    .id();
    let rebound = String::from_utf8(encoded)
        .expect("compatibility fixture is UTF-8")
        .replace(&template.generation.to_string(), &generation.to_string());
    let IrDocument::NormalizedV1_1(document) = decode_ir_document(
        rebound.as_bytes(),
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("generation-rebound fixture decodes") else {
        panic!("fixture uses normalized IR 1.1");
    };
    let metadata = GenerationMetadata::new(
        document.repository,
        generation,
        None,
        manifest_hash,
        configuration_hash,
        provider_set_hash,
    )
    .expect("query fixture metadata is valid");
    GenerationSnapshot::new(
        metadata,
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("query fixture is canonical")
}

fn fallback_fixture(path: &str) -> (GenerationSnapshot, SourceSnapshot) {
    let base = fixture_snapshot();
    let metadata = base.metadata();
    let mut document = base.document().clone();
    let content = b"fallback_identifier { color: green; }\n".to_vec();
    let path = RelativePath::parse(Path::new(path)).expect("synthetic fallback path is valid");
    let file = derive_file(FileIdentity {
        repository: document.repository,
        path_identity: path.identity_bytes(),
    })
    .id();
    let content_hash = content_hash(&content);
    let byte_length = u64::try_from(content.len()).expect("fixture length fits");
    let source = SourceRef::new(
        document.repository,
        document.generation,
        SourceSpan::new(file, 0, byte_length).expect("fixture span is ordered"),
        content_hash,
        None,
    );
    let provenance = document.provenance[0].id;
    document.files.push(FileRecord {
        id: file,
        repository: document.repository,
        generation: document.generation,
        path: path.as_str().to_owned(),
        path_locator: None,
        content_hash,
        byte_length,
        language: "synthetic".to_owned(),
        encoding: "utf-8".to_owned(),
        generated: false,
        provenance,
        evidence: FactEvidence {
            source: Some(source.clone()),
            derivation: Vec::new(),
        },
    });
    document.diagnostics.push(DiagnosticRecord {
        id: FactId::from_bytes([0xee; 20]),
        repository: document.repository,
        generation: document.generation,
        code: "unsupported-language".to_owned(),
        message: "synthetic source has bounded fallback coverage".to_owned(),
        severity: DiagnosticSeverity::Warning,
        source: Some(source.clone()),
        coverage_effect: CoverageStatus::Unknown,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: Vec::new(),
        },
    });
    let snapshot = GenerationSnapshot::new(
        metadata,
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("fallback fixture is canonical");
    let source = SourceSnapshot::from_persisted(
        snapshot.metadata().repository(),
        path,
        file,
        content_hash,
        content,
    )
    .expect("fallback source identity is canonical");
    (snapshot, source)
}

#[test]
fn admitted_metadata_and_hidden_files_retain_global_source_projection() {
    for path in [
        "Cargo.toml",
        "nested/go.mod",
        "package.json",
        "pyproject.toml",
        "requirements.txt",
        "tsconfig.json",
        "src/.gitignore",
        ".settings.json",
        "mystery.sourceblob",
        "normalize.css",
    ] {
        let (snapshot, source) = fallback_fixture(path);
        let projected = project_lexical_documents_with_sources(
            &snapshot,
            &[&source],
            BuildBudget::default(),
            &Cancellation::new(),
        )
        .expect("admitted source projects");
        let files = projected
            .iter()
            .filter(|document| document.file_id == source.file() && document.symbol_id.is_none())
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 1, "{path}");
        assert_eq!(files[0].path, path);
        assert!(
            files[0]
                .source_identifiers
                .iter()
                .any(|identifier| identifier == "fallback_identifier")
        );
        assert!(
            files[0]
                .source_text
                .as_deref()
                .is_some_and(|text| text.contains("fallback_identifier"))
        );
    }
}

#[test]
fn streaming_fallback_projection_matches_batch_and_fails_closed() {
    let (snapshot, source) = fallback_fixture("styles/example.sourceblob");
    let cancellation = Cancellation::new();
    let expected = project_lexical_documents_with_sources(
        &snapshot,
        &[&source],
        BuildBudget::default(),
        &cancellation,
    )
    .expect("batch fallback projection succeeds");
    let unrelated_content = b"unrelated_identifier".to_vec();
    let unrelated_path =
        RelativePath::parse(Path::new("other.sourceblob")).expect("unrelated path is valid");
    let unrelated_file = derive_file(FileIdentity {
        repository: snapshot.metadata().repository(),
        path_identity: unrelated_path.identity_bytes(),
    })
    .id();
    let unrelated_hash = content_hash(&unrelated_content);
    let unrelated = SourceSnapshot::from_persisted(
        snapshot.metadata().repository(),
        unrelated_path,
        unrelated_file,
        unrelated_hash,
        unrelated_content,
    )
    .expect("unrelated source identity is canonical");

    let mut projection =
        LexicalProjectionBuilder::new(&snapshot, BuildBudget::default(), &cancellation)
            .expect("streaming projection starts");
    assert_eq!(projection.next_source_file(), Some(source.file()));
    assert!(matches!(
        projection.push_source(&unrelated, &cancellation),
        Err(QueryError::IndexDrift)
    ));
    assert_eq!(projection.next_source_file(), Some(source.file()));
    projection
        .push_source(&source, &cancellation)
        .expect("canonical fallback source projects");
    assert_eq!(projection.next_source_file(), None);
    let actual = projection
        .finish(&cancellation)
        .expect("complete streaming projection finishes");
    assert_eq!(actual, expected);

    let incomplete =
        LexicalProjectionBuilder::new(&snapshot, BuildBudget::default(), &cancellation)
            .expect("second streaming projection starts");
    assert!(matches!(
        incomplete.finish(&cancellation),
        Err(QueryError::IndexDrift)
    ));
}

fn dispatch_candidate_snapshot() -> (GenerationSnapshot, SymbolId, SymbolId) {
    let base = fixture_snapshot();
    let metadata = base.metadata();
    let mut document = base.document().clone();
    let seed = document.entities[0].id;
    document.entities[0].tier = AnalysisTier::TierD;

    let target = SymbolId::from_bytes([0xa5; 20]);
    let mut target_entity = document.entities[0].clone();
    target_entity.id = target;
    target_entity.canonical_name = "dispatch_target".to_owned();
    target_entity.display_name = "dispatch_target".to_owned();
    target_entity.qualified_name = "crate::dispatch_target".to_owned();
    document.entities.push(target_entity);

    document.relations.push(RelationRecord {
        id: FactId::from_bytes([0xa6; 20]),
        repository: document.repository,
        generation: document.generation,
        subject: RelationEndpoint::Entity(seed),
        predicate: RelationPredicate::DispatchCandidate,
        object: RelationEndpoint::Entity(target),
        confidence: Confidence::new(900).expect("fixture confidence is valid"),
        evidence_kind: EvidenceKind::Syntax,
        provenance: document.provenance[0].id,
        evidence: FactEvidence {
            source: document.entities[0].evidence.source.clone(),
            derivation: Vec::new(),
        },
    });

    let snapshot = GenerationSnapshot::new(
        metadata,
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("dispatch-candidate fixture is canonical");
    (snapshot, seed, target)
}

fn test_and_route_snapshot() -> (GenerationSnapshot, SymbolId, SymbolId, SymbolId, SymbolId) {
    let base = fixture_snapshot();
    let metadata = base.metadata();
    let mut document = base.document().clone();
    let production = document.entities[0].id;
    let source = document.entities[0]
        .evidence
        .source
        .clone()
        .expect("fixture entity has source evidence");
    let provenance = document.provenance[0].id;

    let mut clone_entity = |seed: u8, name: &str, kind| {
        let symbol = SymbolId::from_bytes([seed; 20]);
        let mut entity = document.entities[0].clone();
        entity.id = symbol;
        entity.kind = kind;
        entity.canonical_name = name.to_owned();
        entity.display_name = name.to_owned();
        entity.qualified_name = format!("crate::{name}");
        document.entities.push(entity);
        symbol
    };
    let test = clone_entity(0xb1, "behavior_test", rootlight_ir::EntityKind::Function);
    let handler = clone_entity(0xb2, "handler", rootlight_ir::EntityKind::Function);
    let route = clone_entity(0xb3, "POST /api/generate", rootlight_ir::EntityKind::Route);
    for (id, subject, predicate, object) in [
        (0xb4, test, RelationPredicate::Tests, production),
        (0xb5, handler, RelationPredicate::ServesRoute, route),
    ] {
        document.relations.push(RelationRecord {
            id: FactId::from_bytes([id; 20]),
            repository: document.repository,
            generation: document.generation,
            subject: RelationEndpoint::Entity(subject),
            predicate,
            object: RelationEndpoint::Entity(object),
            confidence: Confidence::new(900).expect("fixture confidence is valid"),
            evidence_kind: EvidenceKind::Derived,
            provenance,
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation: Vec::new(),
            },
        });
    }
    let snapshot = GenerationSnapshot::new(
        metadata,
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("test and route fixture is canonical");
    (snapshot, production, test, handler, route)
}

#[test]
fn plan_change_preserves_the_plan_when_ranked_tests_exceed_output_budget() {
    let base = fixture_snapshot();
    let mut document = base.document().clone();
    let template = document.entities[0].clone();
    let target = template.id;
    for ordinal in 1_u8..=100 {
        let mut test = template.clone();
        let mut identity = [0xf0; 20];
        identity[0] = ordinal;
        test.id = SymbolId::from_bytes(identity);
        test.kind = rootlight_ir::EntityKind::Test;
        test.canonical_name = format!("verification_{ordinal}");
        test.display_name = test.canonical_name.clone();
        test.qualified_name = format!("fixture::{}", test.canonical_name);
        document.entities.push(test);
    }
    let snapshot = GenerationSnapshot::new(
        base.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("ranked-test fixture is valid");
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let execute = |budget| {
        let plan = service
            .plan_plan_change(
                rootlight_query::PlanChangeObjective::BugFix,
                "preserve the selected behavior".to_owned(),
                BTreeSet::from([target]),
                BTreeSet::new(),
                6,
                budget,
            )
            .expect("bounded plan is admitted");
        service.execute_plan_change(&plan, &Cancellation::new())
    };
    let complete = execute(QueryBudget::new()).expect("full plan fits the default policy");
    assert_eq!(complete.data.test_plan.len(), 100);
    let limit = complete.usage.json_bytes / 2;
    for (resource, budget) in [
        (
            QueryResource::Tokens,
            QueryBudget::new().with_max_tokens(limit),
        ),
        (
            QueryResource::JsonBytes,
            QueryBudget::new().with_max_json_bytes(limit),
        ),
    ] {
        let bounded = execute(budget).expect("optional tests must not discard a useful plan");
        assert_eq!(bounded.data.plan, complete.data.plan);
        assert_eq!(bounded.data.affected_scope, complete.data.affected_scope);
        assert_eq!(bounded.data.generation, snapshot.metadata().generation());
        assert_eq!(
            bounded.data.context_pack_request,
            complete.data.context_pack_request
        );
        assert!(!bounded.data.test_plan.is_empty());
        assert!(bounded.data.test_plan.len() < complete.data.test_plan.len());
        assert_eq!(
            bounded.data.test_plan,
            complete.data.test_plan[..bounded.data.test_plan.len()]
        );
        assert!(bounded.data.execution.is_truncated());
        assert!(
            bounded
                .data
                .execution
                .limiting_resources()
                .contains(&resource)
        );
        assert_eq!(
            bounded.data.limiting_resources,
            bounded.data.execution.limiting_resources()
        );
        assert!(bounded.usage.json_bytes <= limit);
        assert_exact_response_accounting(&bounded);
        let mut extended = bounded.clone();
        extended
            .data
            .test_plan
            .push(complete.data.test_plan[extended.data.test_plan.len()].clone());
        assert!(
            serde_json::to_vec(&extended)
                .expect("extended response serializes")
                .len()
                > usize::try_from(limit).expect("fixture limit fits")
        );
    }
    assert_eq!(
        execute(QueryBudget::new())
            .expect("later complete query is unchanged")
            .data,
        complete.data
    );
    assert!(matches!(
        execute(QueryBudget::new().with_max_tokens(1)),
        Err(QueryError::BudgetExceeded {
            resource: QueryResource::Tokens,
            ..
        })
    ));
}

fn selective_relationship_snapshot() -> (GenerationSnapshot, SymbolId, SymbolId) {
    let (base, production, test, _handler, _route) = test_and_route_snapshot();
    let metadata = base.metadata();
    let mut document = base.document().clone();
    let mut desired = document
        .relations
        .iter()
        .find(|relation| relation.predicate == RelationPredicate::Tests)
        .cloned()
        .expect("fixture has one test relationship");
    let mut unrelated = document
        .relations
        .iter()
        .find(|relation| relation.predicate == RelationPredicate::ServesRoute)
        .cloned()
        .expect("fixture has one unrelated route relationship");
    let first = derive_fact("rootlight.query-test.selective-relation/v1", b"first").id();
    let second = derive_fact("rootlight.query-test.selective-relation/v1", b"second").id();
    let (unrelated_id, desired_id) = if first < second {
        (first, second)
    } else {
        (second, first)
    };
    unrelated.id = unrelated_id;
    desired.id = desired_id;
    document.relations = vec![unrelated, desired];
    let snapshot = GenerationSnapshot::new(
        metadata,
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("selective relationship fixture is canonical");
    (snapshot, production, test)
}

fn fixture_search(snapshot: &GenerationSnapshot) -> FakeSearch {
    let entity = &snapshot.document().entities[0];
    let source = entity
        .evidence
        .source
        .as_ref()
        .expect("fixture entity has source evidence");
    let file = snapshot
        .document()
        .files
        .binary_search_by_key(&source.span().file(), |record| record.id)
        .ok()
        .and_then(|index| snapshot.document().files.get(index))
        .expect("fixture source file exists");
    FakeSearch {
        generation: snapshot.metadata().generation(),
        hits: vec![SearchHit {
            symbol_id: Some(entity.id),
            file_id: file.id,
            identifier: entity.display_name.clone(),
            canonical_name: (entity.canonical_name != entity.display_name)
                .then(|| entity.canonical_name.clone()),
            qualified_name: entity.qualified_name.clone(),
            path: file.path.clone(),
            kind: serialized_label(&entity.kind),
            language: entity.language.clone(),
            tier: serialized_label(&entity.tier),
            generated: file.generated,
            test: false,
            declaration_only: false,
            relevance_score: 1.0,
        }],
    }
}

#[test]
fn locate_checks_symbol_language_independently_from_its_host_file() {
    let original = fixture_snapshot();
    let mut document = original.document().clone();
    document.files[0].language = "markdown".to_owned();
    let snapshot = GenerationSnapshot::new(
        original.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    for (language, valid) in [
        (snapshot.document().entities[0].language.as_str(), true),
        ("markdown", false),
        ("unknown", false),
    ] {
        let mut search = fixture_search(&snapshot);
        search.hits[0].language = language.to_owned();
        let service = QueryService::new(&snapshot, &search).unwrap();
        let plan = service
            .plan_code_locate(
                "fixture".to_owned(),
                LocateMode::Exact,
                1,
                0,
                SearchBudget::default(),
                QueryBudget::new(),
            )
            .unwrap();
        let result = service.execute_code_locate(&plan, &Cancellation::new());
        if valid {
            assert!(result.is_ok(), "{result:?}");
        } else {
            assert!(matches!(result, Err(QueryError::IndexDrift)), "{result:?}");
        }
    }
}

fn serialized_label(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value)
        .expect("fixture label serializes")
        .trim_matches('"')
        .to_owned()
}

#[test]
fn locate_and_explain_use_deterministic_typed_plans() {
    let snapshot = fixture_snapshot();
    let projected =
        project_lexical_documents(&snapshot, BuildBudget::default(), &Cancellation::new())
            .expect("normalized entities project into bounded lexical metadata");
    assert_eq!(projected.len(), snapshot.document().entities.len());
    assert!(
        projected
            .iter()
            .all(|document| document.documentation.is_none())
    );
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let cancellation = Cancellation::new();

    let locate = service
        .plan_code_locate(
            search.hits[0].identifier.clone(),
            LocateMode::Exact,
            10,
            0,
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .expect("locate plan is admitted");
    assert_eq!(locate.explanation().kind, PlanKind::CodeLocate);
    assert_eq!(locate.explanation().estimate.json_bytes, 1024 * 1024);
    assert_eq!(locate.explanation().estimate.estimated_tokens, 1_000_000);
    assert_eq!(locate.explanation().estimate.duration_micros, 2_000_000);
    let located = service
        .execute_code_locate(&locate, &cancellation)
        .expect("locate query succeeds");
    assert_eq!(located.data.hits.len(), 1);
    assert_eq!(located.data.matched_candidates, 1);
    assert!(!located.data.truncated);
    assert_eq!(
        located.data.execution.state(),
        ExecutionCompletenessState::Complete
    );
    assert!(located.data.execution.limiting_resources().is_empty());
    assert_eq!(
        located.data.hits[0].trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
    assert!(located.usage.rows >= 2);
    assert!(located.usage.json_bytes > 0);
    assert_exact_response_accounting(&located);

    let filtered = service
        .plan_code_locate_with_languages(
            search.hits[0].identifier.clone(),
            LocateMode::Exact,
            vec!["unknown".to_owned()],
            10,
            0,
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .expect("canonical language filter is admitted");
    let filtered = service
        .execute_code_locate(&filtered, &cancellation)
        .expect("language-filtered query succeeds");
    assert_eq!(filtered.data.matched_candidates, 0);
    assert!(filtered.data.hits.is_empty());

    let explain = service
        .plan_symbol_explain(
            search.hits[0]
                .symbol_id
                .expect("symbol projection has identity"),
            QueryBudget::new(),
        )
        .expect("explain plan is admitted");
    let explained = service
        .execute_symbol_explain(&explain, &cancellation)
        .expect("explain query succeeds");
    assert_eq!(
        explained.data.entity.id,
        search.hits[0]
            .symbol_id
            .expect("symbol projection has identity")
    );
    assert_eq!(
        explained.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
    assert!(explained.usage.results >= 2);
    assert!(!explained.data.truncated);
    assert_eq!(
        explained.data.execution.state(),
        ExecutionCompletenessState::Complete
    );
    assert!(explained.data.execution.limiting_resources().is_empty());
    assert_exact_response_accounting(&explained);
}

#[test]
fn locate_kind_plans_filter_real_backend_and_preserve_source_evidence() {
    let snapshot = fixture_snapshot();
    let cancellation = Cancellation::new();
    let documents =
        project_lexical_documents(&snapshot, BuildBudget::default(), &cancellation).unwrap();
    let identifier = documents[0].identifier.clone();
    let kind = documents[0].kind.clone();
    let index = rootlight_search::LexicalIndex::build_ephemeral(
        snapshot.metadata().generation(),
        documents,
        BuildBudget::default(),
        &cancellation,
    )
    .unwrap();
    let service = QueryService::new(&snapshot, &index).unwrap();
    for (kinds, expected) in [
        (None, 1),
        (Some(vec![kind.clone()]), 1),
        (Some(vec![]), 0),
        (Some(vec!["future_kind".to_owned()]), 0),
    ] {
        let plan = service
            .plan_code_locate_with_entity_filters(
                identifier.clone(),
                LocateMode::Exact,
                vec![],
                vec![],
                kinds,
                1,
                0,
                SearchBudget::default(),
                QueryBudget::new(),
            )
            .unwrap();
        let located = service.execute_code_locate(&plan, &cancellation).unwrap();
        assert_eq!(located.data.hits.len(), expected);
        assert_eq!(
            located.data.matched_candidates,
            u64::try_from(expected).unwrap()
        );
        assert!(!located.data.truncated);
        assert_exact_response_accounting(&located);
        if let Some(hit) = located.data.hits.first() {
            assert_eq!(hit.source, snapshot.document().entities[0].evidence.source);
        }
    }
    let final_page = service
        .plan_code_locate_with_entity_filters(
            identifier.clone(),
            LocateMode::Exact,
            vec![],
            vec![],
            Some(vec![kind.clone()]),
            1,
            1,
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .unwrap();
    let final_page = service
        .execute_code_locate(&final_page, &cancellation)
        .unwrap();
    assert!(final_page.data.hits.is_empty());
    assert_eq!(final_page.data.matched_candidates, 1);
    assert!(!final_page.data.truncated);
    assert!(matches!(
        service.plan_code_locate_with_entity_filters(
            identifier,
            LocateMode::Exact,
            vec![],
            vec![],
            Some(vec![kind.clone(), kind]),
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new()
        ),
        Err(QueryError::Search(SearchError::InvalidKindFilter))
    ));
}

#[test]
fn locate_kind_plan_fails_closed_with_legacy_backend() {
    let snapshot = fixture_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).unwrap();
    for kinds in [vec![], vec![search.hits[0].kind.clone()]] {
        let plan = service
            .plan_code_locate_with_entity_filters(
                search.hits[0].identifier.clone(),
                LocateMode::Exact,
                vec![],
                vec![],
                Some(kinds),
                1,
                0,
                SearchBudget::default(),
                QueryBudget::new(),
            )
            .unwrap();
        assert!(matches!(
            service.execute_code_locate(&plan, &Cancellation::new()),
            Err(QueryError::Search(SearchError::InvalidKindFilter))
        ));
    }
}

#[test]
fn locate_rejects_a_backend_that_ignores_the_requested_kind_union() {
    struct IgnoringKinds(FakeSearch);
    impl LexicalSearch for IgnoringKinds {
        fn generation(&self) -> rootlight_ids::GenerationId {
            self.0.generation
        }
        fn document_count(&self) -> u64 {
            self.0.document_count()
        }
        fn search_with_stats(
            &self,
            request: &SearchRequest,
            budget: SearchBudget,
            cancellation: &Cancellation,
        ) -> Result<SearchOutcome, SearchError> {
            self.0.search_with_stats(request, budget, cancellation)
        }
        fn search_with_language_filter_and_stats(
            &self,
            request: &SearchRequest,
            languages: &[String],
            budget: SearchBudget,
            cancellation: &Cancellation,
        ) -> Result<SearchOutcome, SearchError> {
            self.0
                .search_with_language_filter_and_stats(request, languages, budget, cancellation)
        }
        fn search_with_entity_filters_and_stats(
            &self,
            request: &SearchRequest,
            _filters: rootlight_search::SearchFilters<'_>,
            budget: SearchBudget,
            cancellation: &Cancellation,
        ) -> Result<SearchOutcome, SearchError> {
            self.0.search_with_stats(request, budget, cancellation)
        }
    }
    let snapshot = fixture_snapshot();
    let search = IgnoringKinds(fixture_search(&snapshot));
    let service = QueryService::new(&snapshot, &search).unwrap();
    for kinds in [vec![], vec!["future_kind".to_owned()]] {
        let plan = service
            .plan_code_locate_with_entity_filters(
                search.0.hits[0].identifier.clone(),
                LocateMode::Exact,
                vec![],
                vec![],
                Some(kinds),
                1,
                0,
                SearchBudget::default(),
                QueryBudget::new(),
            )
            .unwrap();
        assert!(matches!(
            service.execute_code_locate(&plan, &Cancellation::new()),
            Err(QueryError::IndexDrift)
        ));
    }
}

#[test]
fn locate_rejects_canonical_aliases_not_proven_by_the_durable_entity() {
    let snapshot = fixture_snapshot();
    let mut search = fixture_search(&snapshot);
    search.hits[0].canonical_name = Some("unrelated_name".to_owned());
    let service = QueryService::new(&snapshot, &search).unwrap();
    let plan = service
        .plan_code_locate(
            "unrelated_name".to_owned(),
            LocateMode::Exact,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .unwrap();
    assert!(matches!(
        service.execute_code_locate(&plan, &Cancellation::new()),
        Err(QueryError::IndexDrift)
    ));
}

#[test]
fn execution_enforces_cancellation_and_exact_output_bounds() {
    let snapshot = fixture_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    let plan = service
        .plan_code_locate(
            "fixture".to_owned(),
            LocateMode::Text,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .expect("plan is admitted");
    assert!(matches!(
        service.execute_code_locate(&plan, &cancelled),
        Err(QueryError::Cancelled(CancellationReason::ClientRequest))
    ));

    let backend_cancelled = BackendCancelledSearch(fixture_search(&snapshot));
    let backend_service =
        QueryService::new(&snapshot, &backend_cancelled).expect("generation inputs agree");
    let backend_plan = backend_service
        .plan_code_locate(
            "fixture".to_owned(),
            LocateMode::Text,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .expect("backend-cancellation plan is admitted");
    assert!(matches!(
        backend_service.execute_code_locate(&backend_plan, &Cancellation::new()),
        Err(QueryError::Cancelled(CancellationReason::ClientRequest))
    ));
    assert!(matches!(
        QueryError::from(rootlight_source::SourceError::Cancelled(
            CancellationReason::DeadlineExceeded,
        )),
        QueryError::Cancelled(CancellationReason::DeadlineExceeded)
    ));

    let tiny_output = service
        .plan_code_locate(
            "fixture".to_owned(),
            LocateMode::Text,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new().with_max_json_bytes(1),
        )
        .expect("output is measured at execution");
    assert!(matches!(
        service.execute_code_locate(&tiny_output, &Cancellation::new()),
        Err(QueryError::BudgetExceeded {
            resource: QueryResource::JsonBytes,
            limit: 1,
        })
    ));

    let underreported = UnderreportedSearch(fixture_search(&snapshot));
    let drift_service =
        QueryService::new(&snapshot, &underreported).expect("generation inputs agree");
    let drift_plan = drift_service
        .plan_code_locate(
            "fixture".to_owned(),
            LocateMode::Text,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .expect("drift fixture plan is admitted");
    assert!(matches!(
        drift_service.execute_code_locate(&drift_plan, &Cancellation::new()),
        Err(QueryError::IndexDrift)
    ));
}

fn signature_snapshot(signatures: &[&str]) -> (GenerationSnapshot, SymbolId) {
    let base = fixture_snapshot();
    let mut document = base.document().clone();
    let entity = &document.entities[0];
    let symbol = entity.id;
    for signature in signatures {
        let evidence = rootlight_ir::LexicalEvidenceV1::from_complete_text(
            rootlight_ir::LexicalEvidenceKind::Signature,
            rootlight_ir::FactRef::Entity(symbol),
            rootlight_ir::LexicalEvidenceFormat::SourceText,
            signature,
        )
        .unwrap();
        document.extensions.push(
            rootlight_ir::new_lexical_evidence_envelope(
                document.repository,
                document.generation,
                entity.provenance,
                entity.evidence.source.clone().unwrap(),
                &evidence,
            )
            .unwrap(),
        );
    }
    (
        GenerationSnapshot::new(
            base.metadata(),
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap(),
        symbol,
    )
}

#[test]
fn explanations_preserve_retained_multiline_signatures_and_truncation() {
    let long = format!("fn large({})", "value: é, ".repeat(600));
    for text in [
        "fn item(\n    value: &str,\n) -> Result<é, Error>",
        long.as_str(),
    ] {
        let (snapshot, symbol) = signature_snapshot(&[text]);
        let search = fixture_search(&snapshot);
        let service = QueryService::new(&snapshot, &search).unwrap();
        let plan = service
            .plan_symbol_explain(symbol, QueryBudget::new())
            .unwrap();
        let result = service
            .execute_symbol_explain(&plan, &Cancellation::new())
            .unwrap();
        let signature = result.data.signature.as_ref().unwrap();
        assert!(text.starts_with(signature.text()));
        assert_eq!(
            signature.is_truncated(),
            text.len() > rootlight_ir::MAX_LEXICAL_SIGNATURE_BYTES
        );
        assert_eq!(
            signature.complete_text_hash(),
            content_hash(text.as_bytes())
        );
        assert!(!result.data.truncated);
        assert_exact_response_accounting(&result);
    }
}

#[test]
fn explanations_do_not_guess_missing_or_conflicting_signatures() {
    for signatures in [Vec::new(), vec!["fn item() -> A", "fn item() -> B"]] {
        let (snapshot, symbol) = signature_snapshot(&signatures);
        let search = fixture_search(&snapshot);
        let service = QueryService::new(&snapshot, &search).unwrap();
        let plan = service
            .plan_symbol_explain(symbol, QueryBudget::new())
            .unwrap();
        let result = service
            .execute_symbol_explain(&plan, &Cancellation::new())
            .unwrap();
        assert!(result.data.signature.is_none());
        assert!(!result.data.truncated);
        assert_exact_response_accounting(&result);
    }
}

#[test]
fn explanations_do_not_return_signatures_from_incomplete_scans() {
    let (snapshot, symbol) = signature_snapshot(&["fn item() -> A"]);
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).unwrap();
    let plan = service
        .plan_symbol_explain(symbol, QueryBudget::new().with_max_rows(2))
        .unwrap();
    let result = service
        .execute_symbol_explain(&plan, &Cancellation::new())
        .unwrap();
    assert!(result.data.signature.is_none());
    assert!(result.data.truncated);
    assert!(
        result
            .data
            .limiting_resources
            .contains(&QueryResource::Rows)
    );
    assert_eq!(result.usage.rows, 2);
    assert_exact_response_accounting(&result);
}

#[test]
fn retained_signatures_obey_source_byte_budgets() {
    let text = "fn item(value: é) -> Output";
    let bytes = u64::try_from(text.len()).unwrap();
    let (snapshot, symbol) = signature_snapshot(&[text]);
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).unwrap();
    for maximum in [bytes - 1, bytes, bytes + 1] {
        let plan = service
            .plan_symbol_explain(symbol, QueryBudget::new().with_max_source_bytes(maximum))
            .unwrap();
        let result = service
            .execute_symbol_explain(&plan, &Cancellation::new())
            .unwrap();
        if maximum < bytes {
            assert!(result.data.signature.is_none());
            assert_eq!(result.usage.source_bytes, 0);
            assert!(result.data.truncated);
            assert!(
                result
                    .data
                    .limiting_resources
                    .contains(&QueryResource::SourceBytes)
            );
        } else {
            assert_eq!(result.data.signature.as_ref().unwrap().text(), text);
            assert_eq!(result.usage.source_bytes, bytes);
            assert!(!result.data.truncated);
        }
        assert!(result.usage.source_bytes <= plan.explanation().estimate.source_bytes);
        assert_exact_response_accounting(&result);
    }
}

#[test]
fn locate_planning_enforces_configured_and_hard_query_byte_boundaries() {
    let snapshot = fixture_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let configured_edge = format!("{}é", "a ".repeat(255));
    let configured_overflow = format!("{configured_edge}a");
    assert_eq!(configured_edge.len(), 512);
    assert_eq!(configured_overflow.len(), 513);

    service
        .plan_code_locate(
            configured_edge,
            LocateMode::Exact,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .expect("configured query-byte boundary is admitted");
    assert!(matches!(
        service.plan_code_locate(
            configured_overflow,
            LocateMode::Exact,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new(),
        ),
        Err(QueryError::Search(SearchError::InvalidQuery(
            QueryViolation::TooLong
        )))
    ));

    let hard_budget = SearchBudget {
        max_query_bytes: 4_096,
        ..SearchBudget::default()
    };
    let hard_edge = format!("{}é", "a ".repeat(2_047));
    let hard_overflow = format!("{hard_edge}a");
    assert_eq!(hard_edge.len(), 4_096);
    assert_eq!(hard_overflow.len(), 4_097);
    service
        .plan_code_locate(
            hard_edge,
            LocateMode::Exact,
            1,
            0,
            hard_budget,
            QueryBudget::new(),
        )
        .expect("hard query-byte boundary is admitted");
    assert!(matches!(
        service.plan_code_locate(
            hard_overflow,
            LocateMode::Exact,
            1,
            0,
            hard_budget,
            QueryBudget::new(),
        ),
        Err(QueryError::Search(SearchError::InvalidQuery(
            QueryViolation::TooLong
        )))
    ));
}

#[test]
fn bounded_queries_mark_deterministic_partial_results() {
    let snapshot = fixture_snapshot();
    let search = fixture_search(&snapshot);
    let truncated_search = TruncatedSearch(search.clone());
    let locate_service =
        QueryService::new(&snapshot, &truncated_search).expect("generation inputs agree");
    let locate_plan = locate_service
        .plan_code_locate(
            search.hits[0].identifier.clone(),
            LocateMode::Exact,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .expect("bounded locate plan is admitted");
    let located = locate_service
        .execute_code_locate(&locate_plan, &Cancellation::new())
        .expect("bounded locate returns a partial prefix");
    assert!(located.data.truncated);
    assert_eq!(
        located.data.execution.state(),
        ExecutionCompletenessState::Truncated
    );
    assert_eq!(located.data.matched_candidates, 2);
    assert_eq!(
        located.data.limiting_resources,
        vec![QueryResource::Results]
    );
    assert_eq!(
        located.data.execution.limiting_resources(),
        located.data.limiting_resources
    );

    let explain_service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let explain_plan = explain_service
        .plan_symbol_explain(
            search.hits[0]
                .symbol_id
                .expect("symbol projection has identity"),
            QueryBudget::new().with_max_rows(2),
        )
        .expect("mandatory explain records fit");
    let first = explain_service
        .execute_symbol_explain(&explain_plan, &Cancellation::new())
        .expect("large-neighborhood scan returns a partial result");
    let second = explain_service
        .execute_symbol_explain(&explain_plan, &Cancellation::new())
        .expect("repeated partial scan succeeds");
    assert!(first.data.truncated);
    assert_eq!(
        first.data.execution.state(),
        ExecutionCompletenessState::Truncated
    );
    assert_eq!(first.data.limiting_resources, vec![QueryResource::Rows]);
    assert_eq!(
        first.data.execution.limiting_resources(),
        first.data.limiting_resources
    );
    assert_eq!(first.data, second.data);
    assert_eq!(first.usage.rows, 2);
}

#[test]
fn code_locate_hard_coverage_limit_suppresses_page_continuation() {
    let snapshot = fixture_snapshot();
    assert!(
        !snapshot.document().coverage_records.is_empty(),
        "fixture must exercise coverage scanning"
    );
    let search = fixture_search(&snapshot);
    let truncated_search = TruncatedSearch(search.clone());
    let service = QueryService::new(&snapshot, &truncated_search).expect("generation inputs agree");
    let plan = service
        .plan_code_locate(
            search.hits[0].identifier.clone(),
            LocateMode::Exact,
            1,
            0,
            SearchBudget {
                max_candidates: 2,
                ..SearchBudget::default()
            },
            QueryBudget::new().with_max_rows(3),
        )
        .expect("the search page itself fits the row budget");
    let response = service
        .execute_code_locate(&plan, &Cancellation::new())
        .expect("hard coverage truncation returns an honest partial result");

    assert!(response.data.truncated);
    assert_eq!(
        response.data.execution.state(),
        ExecutionCompletenessState::Truncated
    );
    assert_eq!(response.data.next_page_offset, None);
    assert_eq!(response.data.limiting_resources, vec![QueryResource::Rows]);
    assert_eq!(
        response.data.execution.limiting_resources(),
        response.data.limiting_resources
    );
}

#[test]
fn architecture_overview_returns_scoped_truncation_when_workspace_is_unfunded() {
    let snapshot = fixture_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let plan = service
        .plan_architecture_overview(
            Vec::new(),
            0,
            50,
            true,
            QueryBudget::new().with_max_memory_bytes(1),
        )
        .expect("the bounded overview plan is admitted");

    let response = service
        .execute_architecture_overview(&plan, &Cancellation::new())
        .expect("workspace exhaustion returns an honest partial result");

    assert_eq!(response.plan.kind, PlanKind::ArchitectureOverview);
    assert_eq!(
        response.data.execution.state(),
        ExecutionCompletenessState::Truncated
    );
    assert_eq!(
        response.data.execution.limiting_resources(),
        &[QueryResource::MemoryBytes]
    );
    assert_eq!(
        response.data.limiting_resources,
        vec![QueryResource::MemoryBytes]
    );
    assert!(response.data.components.is_empty());
    assert!(response.data.connections.is_empty());
    assert!(response.data.hotspots.is_empty());
    assert!(response.data.communities.is_empty());
    assert!(response.data.views.is_empty());
    assert_eq!(response.usage.rows, 0);
    assert_eq!(response.usage.edges, 0);
    assert_eq!(response.usage.memory_bytes, 0);
    assert_exact_response_accounting(&response);
}

#[test]
fn architecture_cycles_returns_scoped_truncation_when_workspace_is_unfunded() {
    let snapshot = fixture_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let plan = service
        .plan_architecture_cycles(
            vec![RelationFamily::Calls],
            0,
            2,
            1,
            false,
            QueryBudget::new().with_max_memory_bytes(1),
        )
        .expect("the bounded cycle plan is admitted");

    let response = service
        .execute_architecture_cycles(&plan, &Cancellation::new())
        .expect("workspace exhaustion returns an honest partial result");

    assert_eq!(response.plan.kind, PlanKind::ArchitectureCycles);
    assert_eq!(
        response.data.execution.state(),
        ExecutionCompletenessState::Truncated
    );
    assert_eq!(
        response.data.execution.limiting_resources(),
        &[QueryResource::MemoryBytes]
    );
    assert_eq!(
        response.data.limiting_resources,
        vec![QueryResource::MemoryBytes]
    );
    assert!(response.data.components.is_empty());
    assert!(response.data.cycles.is_empty());
    assert!(response.data.break_candidates.is_empty());
    assert_eq!(response.usage.rows, 0);
    assert_eq!(response.usage.edges, 0);
    assert_eq!(response.usage.memory_bytes, 0);
    assert_exact_response_accounting(&response);
}

#[test]
fn analytical_queries_return_scoped_truncation_when_workspace_is_unfunded() {
    let snapshot = fixture_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let symbol = search.hits[0]
        .symbol_id
        .expect("symbol projection has identity");
    let budget = QueryBudget::new().with_max_memory_bytes(1);

    let flow_plan = service
        .plan_flow_trace(
            symbol,
            None,
            Some(RelationDirection::Both),
            vec![RelationFamily::Calls],
            0,
            1,
            1,
            budget,
        )
        .expect("the bounded flow plan is admitted");
    let flow = service
        .execute_flow_trace(&flow_plan, &Cancellation::new())
        .expect("flow workspace exhaustion returns an honest partial result");
    assert_eq!(flow.plan.kind, PlanKind::FlowTrace);
    assert_eq!(
        flow.data.execution.state(),
        ExecutionCompletenessState::Truncated
    );
    assert_eq!(
        flow.data.execution.limiting_resources(),
        &[QueryResource::MemoryBytes]
    );
    assert_eq!(
        flow.data.limiting_resources,
        vec![QueryResource::MemoryBytes]
    );
    assert!(flow.data.paths.is_empty());
    assert!(flow.data.frontier.truncated);
    assert_eq!(flow.usage.rows, 0);
    assert_eq!(flow.usage.edges, 0);
    assert_eq!(flow.usage.memory_bytes, 0);
    assert_exact_response_accounting(&flow);

    let impact_plan = service
        .plan_change_impact(BTreeSet::from([symbol]), Vec::new(), 1, 0, false, 1, budget)
        .expect("the bounded impact plan is admitted");
    let impact = service
        .execute_change_impact(&impact_plan, &Cancellation::new())
        .expect("impact workspace exhaustion returns an honest partial result");
    assert_eq!(impact.plan.kind, PlanKind::ChangeImpact);
    assert_eq!(
        impact.data.execution.state(),
        ExecutionCompletenessState::Truncated
    );
    assert_eq!(
        impact.data.execution.limiting_resources(),
        &[QueryResource::MemoryBytes]
    );
    assert_eq!(
        impact.data.limiting_resources,
        vec![QueryResource::MemoryBytes]
    );
    assert!(impact.data.resolved_changes.is_empty());
    assert!(impact.data.impacted.is_empty());
    assert!(impact.data.tests.is_empty());
    assert_eq!(impact.data.risk_summary.coverage, CoverageStatus::Bounded);
    assert!(impact.data.risk_summary.dynamic_blind_spots);
    assert_eq!(impact.usage.rows, 0);
    assert_eq!(impact.usage.edges, 0);
    assert_eq!(impact.usage.memory_bytes, 0);
    assert_exact_response_accounting(&impact);

    let history_plan = service
        .plan_history_compare(snapshot.metadata().generation(), BTreeSet::new(), 1, budget)
        .expect("the bounded history plan is admitted");
    let history = service
        .execute_history_compare(&history_plan, snapshot.document(), &Cancellation::new())
        .expect("history workspace exhaustion returns an honest partial result");
    assert_eq!(history.plan.kind, PlanKind::HistoryCompare);
    assert_eq!(
        history.data.execution.state(),
        ExecutionCompletenessState::Truncated
    );
    assert_eq!(
        history.data.execution.limiting_resources(),
        &[QueryResource::MemoryBytes]
    );
    assert_eq!(
        history.data.limiting_resources,
        vec![QueryResource::MemoryBytes]
    );
    assert_eq!(history.data.coverage, CoverageStatus::Complete);
    assert!(history.data.changes.is_empty());
    assert!(history.data.breaking_candidates.is_empty());
    assert!(history.data.lineage.is_empty());
    assert_eq!(history.usage.rows, 0);
    assert_eq!(history.usage.edges, 0);
    assert_eq!(history.usage.memory_bytes, 0);
    assert_exact_response_accounting(&history);
}

#[test]
fn plans_and_execution_enforce_all_query_resource_families() {
    let snapshot = fixture_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let symbol = search.hits[0]
        .symbol_id
        .expect("symbol projection has identity");

    assert!(matches!(
        service.plan_code_locate(
            "fixture".to_owned(),
            LocateMode::Text,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new().with_max_rows(0),
        ),
        Err(QueryError::InvalidBudget {
            resource: QueryResource::Rows,
            ..
        })
    ));
    assert!(matches!(
        service.plan_symbol_explain(symbol, QueryBudget::new().with_max_duration(Duration::ZERO)),
        Err(QueryError::InvalidDurationBudget { .. })
    ));
    assert!(matches!(
        service.plan_symbol_explain(
            symbol,
            QueryBudget::new().with_max_duration(Duration::from_secs(11)),
        ),
        Err(QueryError::InvalidDurationBudget { maximum })
            if maximum == Duration::from_secs(10)
    ));
    assert!(matches!(
        service.plan_symbol_explain(
            symbol,
            QueryBudget::new().with_max_json_bytes(4 * 1024 * 1024 + 1),
        ),
        Err(QueryError::InvalidBudget {
            resource: QueryResource::JsonBytes,
            maximum,
        }) if maximum == 4 * 1024 * 1024
    ));
    assert!(matches!(
        service.plan_symbol_explain(
            symbol,
            QueryBudget::new().with_max_tokens(4 * 1024 * 1024 + 1),
        ),
        Err(QueryError::InvalidBudget {
            resource: QueryResource::Tokens,
            maximum,
        }) if maximum == 4 * 1024 * 1024
    ));
    assert!(matches!(
        service.plan_code_locate(
            "fixture".to_owned(),
            LocateMode::Text,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new().with_max_rows(1),
        ),
        Err(QueryError::PlanRejected {
            resource: QueryResource::Rows,
        })
    ));
    assert!(matches!(
        service.plan_symbol_explain(symbol, QueryBudget::new().with_max_results(1)),
        Err(QueryError::PlanRejected {
            resource: QueryResource::Results,
        })
    ));

    let exact_edge_budget = service
        .plan_symbol_explain(symbol, QueryBudget::new().with_max_edges(1))
        .expect("fixture has exactly one bounded relation");
    let explained = service
        .execute_symbol_explain(&exact_edge_budget, &Cancellation::new())
        .expect("exact edge boundary succeeds");
    assert_eq!(explained.usage.edges, 1);
    assert!(!explained.data.truncated);

    let memory_limited = service
        .plan_symbol_explain(symbol, QueryBudget::new().with_max_memory_bytes(1))
        .expect("runtime measures the exact response memory");
    assert!(matches!(
        service.execute_symbol_explain(&memory_limited, &Cancellation::new()),
        Err(QueryError::BudgetExceeded {
            resource: QueryResource::MemoryBytes,
            limit: 1,
        })
    ));

    let token_limited = service
        .plan_code_locate(
            "fixture".to_owned(),
            LocateMode::Text,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new().with_max_tokens(1),
        )
        .expect("runtime measures the serialized token estimate");
    assert!(matches!(
        service.execute_code_locate(&token_limited, &Cancellation::new()),
        Err(QueryError::BudgetExceeded {
            resource: QueryResource::Tokens,
            limit: 1,
        })
    ));

    let deadline_limited = service
        .plan_code_locate(
            "fixture".to_owned(),
            LocateMode::Text,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new().with_max_duration(Duration::from_nanos(1)),
        )
        .expect("positive duration is admitted");
    assert!(matches!(
        service.execute_code_locate(&deadline_limited, &Cancellation::new()),
        Err(QueryError::Cancelled(CancellationReason::DeadlineExceeded))
    ));
}

#[test]
fn symbol_relationships_enforces_plan_and_serialization_limits() {
    let snapshot = fixture_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let seeds = BTreeSet::from([search.hits[0]
        .symbol_id
        .expect("symbol projection has identity")]);

    assert!(matches!(
        service.plan_symbol_relationships(
            seeds.clone(),
            vec![RelationFamily::Calls],
            None,
            0,
            2,
            0,
            QueryBudget::new().with_max_results(1),
        ),
        Err(QueryError::PlanRejected {
            resource: QueryResource::Results,
        })
    ));

    let token_limited = service
        .plan_symbol_relationships(
            seeds,
            vec![RelationFamily::Calls],
            None,
            0,
            1,
            0,
            QueryBudget::new().with_max_results(1).with_max_tokens(1),
        )
        .expect("the bounded relationships plan is admitted");
    assert!(matches!(
        service.execute_symbol_relationships(&token_limited, &Cancellation::new()),
        Err(QueryError::BudgetExceeded {
            resource: QueryResource::Tokens,
            limit: 1,
        })
    ));
}

#[test]
fn symbol_relationships_returns_tier_d_dispatch_as_weak_non_exact_call() {
    let (snapshot, seed, target) = dispatch_candidate_snapshot();
    let mut search = fixture_search(&snapshot);
    let seed_entity = snapshot
        .document()
        .entities
        .iter()
        .find(|entity| entity.id == seed)
        .expect("seed entity exists");
    search.hits[0].symbol_id = Some(seed);
    search.hits[0].identifier = seed_entity.display_name.clone();
    search.hits[0].qualified_name = seed_entity.qualified_name.clone();
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let plan = service
        .plan_symbol_relationships(
            BTreeSet::from([seed]),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            10,
            0,
            QueryBudget::new(),
        )
        .expect("candidate call relationship plan is admitted");

    let response = service
        .execute_symbol_relationships(&plan, &Cancellation::new())
        .expect("candidate call relationship query succeeds");

    assert!(!response.data.exact);
    assert!(!response.data.truncated);
    assert_eq!(response.data.returned_edges, 1);
    assert_eq!(response.data.total_edges, 1);
    assert_eq!(response.data.groups.len(), 1);
    let group = &response.data.groups[0];
    assert_eq!(group.seed, seed);
    assert_eq!(group.family, RelationFamily::Calls);
    assert_eq!(group.direction, RelationDirection::Outbound);
    assert_eq!(group.items.len(), 1);
    assert_eq!(group.items[0].symbol, target);
    assert_eq!(group.items[0].confidence, 399);
    assert_eq!(group.items[0].source_refs.len(), 1);
}

fn overlapping_relationship_snapshot() -> (GenerationSnapshot, SymbolId, SymbolId) {
    let (base, seed, target) = dispatch_candidate_snapshot();
    let mut document = base.document().clone();
    let template = document
        .relations
        .iter()
        .find(|relation| relation.predicate == RelationPredicate::DispatchCandidate)
        .expect("fixture contains a candidate call")
        .clone();
    document.relations.clear();
    for (id, confidence) in [(10, 900), (11, 800), (12, 700), (13, 600), (14, 500)] {
        let mut relation = template.clone();
        relation.id = FactId::from_bytes([id; 20]);
        relation.confidence = Confidence::new(confidence).expect("fixture confidence is valid");
        if id != 10 {
            relation.predicate = RelationPredicate::Calls;
        }
        if id == 12 {
            let source = relation
                .evidence
                .source
                .as_ref()
                .expect("call has evidence");
            relation.evidence.source = Some(SourceRef::new(
                source.repository(),
                source.generation(),
                SourceSpan::new(source.span().file(), 0, 1).expect("distinct site is valid"),
                source.content_hash(),
                None,
            ));
        }
        if id >= 13 {
            relation.evidence_kind = EvidenceKind::Derived;
            relation.evidence.source = None;
            relation.evidence.derivation =
                vec![rootlight_ir::FactRef::Fact(FactId::from_bytes([10; 20]))];
        }
        document.relations.push(relation);
    }
    let snapshot = GenerationSnapshot::new(
        base.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("overlapping relationships are canonical");
    (snapshot, seed, target)
}

#[test]
fn symbol_relationships_coalesces_only_identical_sourced_targets_before_paging() {
    let (snapshot, seed, target) = overlapping_relationship_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let execute = |offset, limit, minimum| {
        let plan = service
            .plan_symbol_relationships(
                BTreeSet::from([seed]),
                vec![RelationFamily::Calls],
                Some(RelationDirection::Outbound),
                minimum,
                limit,
                offset,
                QueryBudget::new(),
            )
            .expect("relationships plan is admitted");
        service
            .execute_symbol_relationships(&plan, &Cancellation::new())
            .expect("query succeeds")
    };
    let full = execute(0, 10, 0);
    assert_eq!(
        full.usage.edges, 5,
        "all physical candidates remain charged"
    );
    assert_eq!(full.data.total_edges, 4);
    assert_eq!(full.data.returned_edges, 4);
    assert!(!full.data.exact, "coalescing must not upgrade uncertainty");
    assert!(!full.data.truncated);
    let group = &full.data.groups[0];
    assert_eq!(group.total_count, 4);
    assert!(group.items.iter().all(|item| item.symbol == target));
    assert_eq!(
        group
            .items
            .iter()
            .map(|item| item.confidence)
            .collect::<Vec<_>>(),
        vec![800, 700, 600, 500]
    );
    assert_ne!(group.items[0].source_refs, group.items[1].source_refs);
    assert!(
        group.items[2..]
            .iter()
            .all(|item| item.source_refs.is_empty())
    );
    let first = execute(0, 2, 0);
    let second = execute(2, 2, 0);
    assert_eq!(first.data.total_edges, 4);
    assert_eq!(first.data.groups[0].total_count, 4);
    assert_eq!(first.data.next_page_offset, Some(2));
    assert_eq!(second.data.next_page_offset, None);
    assert_eq!(first.data.groups[0].items, group.items[..2]);
    assert_eq!(second.data.groups[0].items, group.items[2..]);
    assert_eq!(execute(0, 10, 800).data.returned_edges, 1);
}

#[test]
fn symbol_relationships_coalescing_preserves_scan_limits_and_lower_bounds() {
    let (snapshot, seed, _) = overlapping_relationship_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let first_relation_bytes = u64::try_from(
        serde_json::to_vec(&snapshot.document().relations[0])
            .expect("relation encodes")
            .len(),
    )
    .expect("fixture length fits");
    for (budget, resource, expected) in [
        (
            QueryBudget::new().with_max_edges(2),
            QueryResource::Edges,
            1,
        ),
        (QueryBudget::new().with_max_rows(2), QueryResource::Rows, 1),
        (
            QueryBudget::new().with_max_memory_bytes(first_relation_bytes),
            QueryResource::MemoryBytes,
            1,
        ),
        (
            QueryBudget::new().with_max_memory_bytes(1),
            QueryResource::MemoryBytes,
            0,
        ),
    ] {
        let plan = service
            .plan_symbol_relationships(
                BTreeSet::from([seed]),
                vec![RelationFamily::Calls],
                Some(RelationDirection::Outbound),
                0,
                10,
                0,
                budget,
            )
            .expect("bounded plan is admitted");
        let response = service
            .execute_symbol_relationships(&plan, &Cancellation::new())
            .expect("bounded query succeeds");
        assert!(response.data.truncated);
        assert!(!response.data.exact);
        assert!(response.data.limiting_resources.contains(&resource));
        assert_eq!(response.data.next_page_offset, None);
        assert_eq!(response.data.total_edges, expected);
        assert_eq!(response.data.returned_edges, expected);
    }
}

#[test]
fn symbol_relationships_coalescing_keeps_seed_family_and_direction_groups_separate() {
    let (snapshot, seed, target) = overlapping_relationship_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let plan = service
        .plan_symbol_relationships(
            BTreeSet::from([seed, target]),
            vec![RelationFamily::Calls, RelationFamily::CalledBy],
            Some(RelationDirection::Both),
            0,
            20,
            0,
            QueryBudget::new(),
        )
        .expect("multi-seed plan is admitted");
    let response = service
        .execute_symbol_relationships(&plan, &Cancellation::new())
        .expect("query succeeds");
    assert_eq!(response.usage.edges, 20);
    assert_eq!(response.data.total_edges, 16);
    assert_eq!(response.data.returned_edges, 16);
    assert_eq!(response.data.groups.len(), 4);
    for group in &response.data.groups {
        assert_eq!(group.total_count, 4);
        let (direction, counterpart) = if group.seed == seed {
            (RelationDirection::Outbound, target)
        } else {
            assert_eq!(group.seed, target);
            (RelationDirection::Inbound, seed)
        };
        assert_eq!(group.direction, direction);
        assert!(group.items.iter().all(|item| item.symbol == counterpart));
    }
}

#[test]
fn symbol_relationships_coalescing_requires_the_full_source_reference() {
    let (base, seed, _) = overlapping_relationship_snapshot();
    let mut document = base.document().clone();
    let candidate = document
        .relations
        .iter_mut()
        .find(|relation| relation.predicate == RelationPredicate::DispatchCandidate)
        .expect("candidate exists");
    let source = candidate
        .evidence
        .source
        .as_ref()
        .expect("candidate has evidence");
    candidate.evidence.source = Some(SourceRef::new(
        source.repository(),
        source.generation(),
        source.span(),
        source.content_hash(),
        Some(rootlight_ir::LineRange::new(1, 2).expect("line hint is valid")),
    ));
    let snapshot = GenerationSnapshot::new(
        base.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("source hints are valid");
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let plan = service
        .plan_symbol_relationships(
            BTreeSet::from([seed]),
            vec![RelationFamily::Calls],
            None,
            0,
            10,
            0,
            QueryBudget::new(),
        )
        .expect("plan is admitted");
    let response = service
        .execute_symbol_relationships(&plan, &Cancellation::new())
        .expect("query succeeds");
    assert_eq!(response.data.total_edges, 5);
    assert_eq!(response.data.returned_edges, 5);
}

#[test]
fn symbol_relationships_preserves_test_and_route_direction_counterparts_and_sources() {
    let (snapshot, production, test, handler, route) = test_and_route_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");

    for (seed, family, direction, expected) in [
        (
            production,
            RelationFamily::Tests,
            RelationDirection::Inbound,
            test,
        ),
        (
            handler,
            RelationFamily::CallsRoute,
            RelationDirection::Outbound,
            route,
        ),
    ] {
        let plan = service
            .plan_symbol_relationships(
                BTreeSet::from([seed]),
                vec![family],
                Some(direction),
                0,
                10,
                0,
                QueryBudget::new(),
            )
            .expect("reviewed relationship plan is admitted");
        let response = service
            .execute_symbol_relationships(&plan, &Cancellation::new())
            .expect("reviewed relationship query succeeds");
        let group = response
            .data
            .groups
            .first()
            .expect("one relationship group is returned");
        assert_eq!(group.seed, seed);
        assert_eq!(group.family, family);
        assert_eq!(group.direction, direction);
        assert_eq!(group.items.len(), 1);
        assert_eq!(group.items[0].symbol, expected);
        assert_eq!(group.items[0].source_refs.len(), 1);
    }
}

#[test]
fn seed_scoped_queries_do_not_spend_edge_budget_on_unrelated_relations() {
    let (snapshot, production, test) = selective_relationship_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");

    let relationships = service
        .plan_symbol_relationships(
            BTreeSet::from([production]),
            vec![RelationFamily::Tests],
            Some(RelationDirection::Inbound),
            0,
            1,
            0,
            QueryBudget::new().with_max_edges(1),
        )
        .and_then(|plan| service.execute_symbol_relationships(&plan, &Cancellation::new()))
        .expect("the relevant edge fits the exact edge budget");
    assert!(!relationships.data.truncated);
    assert_eq!(relationships.usage.edges, 1);
    assert_eq!(relationships.data.returned_edges, 1);
    assert_eq!(relationships.data.total_edges, 1);
    assert_eq!(relationships.data.groups[0].items[0].symbol, test);

    let row_bounded = service
        .plan_symbol_relationships(
            BTreeSet::from([production]),
            vec![RelationFamily::Tests],
            Some(RelationDirection::Inbound),
            0,
            1,
            0,
            QueryBudget::new().with_max_rows(1),
        )
        .and_then(|plan| service.execute_symbol_relationships(&plan, &Cancellation::new()))
        .expect("an unrelated canonical prefix does not hide a matching edge");
    assert!(row_bounded.data.truncated);
    assert_eq!(
        row_bounded.data.limiting_resources,
        vec![QueryResource::Rows]
    );
    assert_eq!(row_bounded.usage.rows, 1);
    assert_eq!(row_bounded.data.returned_edges, 1);
    assert_eq!(row_bounded.data.total_edges, 1);
    assert_eq!(row_bounded.data.groups[0].items[0].symbol, test);

    let explanation = service
        .plan_symbol_explain(production, QueryBudget::new().with_max_edges(1))
        .and_then(|plan| service.execute_symbol_explain(&plan, &Cancellation::new()))
        .expect("symbol explanation ignores unrelated edges");
    assert!(!explanation.data.truncated);
    assert_eq!(explanation.usage.edges, 1);
    assert_eq!(explanation.data.relations.len(), 1);
    assert_eq!(
        explanation.data.relations[0].predicate,
        RelationPredicate::Tests
    );
}

#[test]
fn source_plan_reads_only_verified_generation_bound_bytes() {
    let current = std::env::current_dir().expect("current directory is available");
    let temporary = tempdir_in(current).expect("local temporary directory is available");
    let content = b"alpha\nbeta\ngamma\n";
    let repository = derive_repository(b"query-source-fixture").id();
    let root = RepositoryRoot::open(repository, temporary.path())
        .expect("fixture root is capability-safe");
    let path = RelativePath::parse(Path::new("sample.rs")).expect("fixture path is canonical");
    fs::write(temporary.path().join("sample.rs"), content).expect("fixture source is written");
    let manifest_hash = content_hash(b"query-source-manifest");
    let configuration_hash = content_hash(b"query-source-configuration");
    let provider_set_hash = content_hash(b"query-source-providers");
    let generation = derive_generation(GenerationIdentity {
        repository,
        parent: None,
        manifest_hash,
        config_hash: configuration_hash,
        provider_set_hash,
        format_version: generation_format_version(),
    })
    .id();
    let file = root.file_id(&path);
    let hash = content_hash(content);
    let full_source = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(
            file,
            0,
            u64::try_from(content.len()).expect("fixture source length fits"),
        )
        .expect("full source span is valid"),
        hash,
        None,
    );
    let selected = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(file, 6, 10).expect("selected source span is valid"),
        hash,
        None,
    );
    let provenance_id = derive_fact("rootlight.query-test.provenance/v1", b"fixture").id();
    let producer = ProducerIdentity::new("rootlight-query-test", "1.0.0", configuration_hash)
        .expect("fixture producer is valid");
    let provenance = ProvenanceRecord {
        id: provenance_id,
        repository,
        generation,
        producer_kind: ProducerKind::Rule,
        producer,
        binary_digest: content_hash(b"query-source-binary"),
        frontend_version: Some("1.0.0".to_owned()),
        language: "rust".to_owned(),
        tier: AnalysisTier::TierB,
        build_context: BuildContextIdentity::new(content_hash(b"query-source-build")),
        input_sources: vec![full_source.clone()],
        evidence_sources: vec![full_source.clone()],
        derivation_parents: Vec::new(),
        rule: Some("fixture".to_owned()),
    };
    let mut document = NormalizedIrDocument::empty(repository, generation);
    document.provenance.push(provenance);
    document.files.push(FileRecord {
        id: file,
        repository,
        generation,
        path: path.as_str().to_owned(),
        path_locator: Some(path.to_locator()),
        content_hash: hash,
        byte_length: u64::try_from(content.len()).expect("fixture source length fits"),
        language: "rust".to_owned(),
        encoding: "utf-8".to_owned(),
        generated: false,
        provenance: provenance_id,
        evidence: FactEvidence {
            source: Some(full_source),
            derivation: Vec::new(),
        },
    });
    let metadata = GenerationMetadata::new(
        repository,
        generation,
        None,
        manifest_hash,
        configuration_hash,
        provider_set_hash,
    )
    .expect("fixture metadata is valid");
    let snapshot = GenerationSnapshot::new(
        metadata,
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("fixture generation is canonical");
    let retained_source =
        SourceSnapshot::from_persisted(repository, path.clone(), file, hash, content.to_vec())
            .expect("retained source identity is canonical");
    let cancellation = Cancellation::new();
    let legacy = project_lexical_documents(&snapshot, BuildBudget::default(), &cancellation)
        .expect("legacy projection remains source-free without unsupported diagnostics");
    assert!(legacy.is_empty());
    let projected = project_lexical_documents_with_all_sources(
        &snapshot,
        &[&retained_source],
        BuildBudget::default(),
        &cancellation,
    )
    .expect("supported source contributes global lexical evidence");
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].symbol_id, None);
    assert_eq!(projected[0].file_id, file);
    assert_eq!(projected[0].language, "rust");
    assert_eq!(
        projected[0].source_text.as_deref(),
        Some("alpha beta gamma")
    );
    assert!(matches!(
        project_lexical_documents_with_all_sources(
            &snapshot,
            &[],
            BuildBudget::default(),
            &cancellation,
        ),
        Err(QueryError::IndexDrift)
    ));
    let search = FakeSearch {
        generation,
        hits: Vec::new(),
    };
    let query = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let source = SourceService::new(&root, &snapshot).expect("source inputs agree");
    assert!(matches!(
        query.plan_source_read(
            vec![selected.clone()],
            SourceReadOptions::new(),
            SourceBudget::new()
                .with_max_source_bytes(32)
                .with_max_response_memory_bytes(1024),
            QueryBudget::new()
                .with_max_source_bytes(4)
                .with_max_memory_bytes(2048),
        ),
        Err(QueryError::PlanRejected {
            resource: QueryResource::SourceBytes,
        })
    ));
    let bounded = query
        .plan_source_read(
            vec![selected.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            SourceBudget::new()
                .with_max_source_bytes(32)
                .with_max_response_memory_bytes(1024),
            QueryBudget::new()
                .with_max_source_bytes(32)
                .with_max_memory_bytes(1024),
        )
        .expect("source plan reserves chunk metadata inside the query memory ceiling");
    let bounded_result = query
        .execute_source_read(&bounded, &source, &Cancellation::new())
        .expect("source query respects the shared memory ceiling");
    assert_eq!(bounded_result.data.chunks[0].bytes, b"beta");
    let plan = query
        .plan_source_read(
            vec![selected],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            SourceBudget::new()
                .with_max_source_bytes(32)
                .with_max_response_memory_bytes(1024)
                .with_max_duration(Duration::from_secs(1)),
            QueryBudget::new()
                .with_max_source_bytes(32)
                .with_max_memory_bytes(2048),
        )
        .expect("source plan is admitted");
    let result = query
        .execute_source_read(&plan, &source, &Cancellation::new())
        .expect("source query succeeds");
    assert_eq!(result.data.chunks[0].bytes, b"beta");
    assert_eq!(result.usage.source_bytes, 4);
    assert_eq!(
        result.data.chunks[0].trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
    assert_exact_response_accounting(&result);
}

#[test]
fn retained_old_generation_remains_addressable_after_activation() {
    let first = verified_empty_generation(1);
    let first_id = first.metadata().generation();
    let second = verified_empty_generation(2);
    let second_id = second.metadata().generation();
    let mut generations = GenerationSet::new(2).expect("retention bound is valid");
    generations
        .publish(
            first,
            FakeSearch {
                generation: first_id,
                hits: Vec::new(),
            },
            true,
        )
        .expect("first generation publishes");
    generations
        .publish(
            second,
            FakeSearch {
                generation: second_id,
                hits: Vec::new(),
            },
            true,
        )
        .expect("second generation publishes");

    assert_eq!(generations.active_generation(), Some(second_id));
    assert_eq!(generations.len(), 2);
    let old = generations
        .query(first_id)
        .expect("old pinned generation remains queryable");
    let plan = old
        .plan_code_locate(
            "absent".to_owned(),
            LocateMode::Exact,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .expect("old generation plan is admitted");
    let result = old
        .execute_code_locate(&plan, &Cancellation::new())
        .expect("old generation query is consistent");
    assert_eq!(result.data.generation, first_id);
    assert!(result.data.hits.is_empty());
}

#[test]
fn unloaded_generation_retains_identity_and_reloads_after_query_lease_releases() {
    let first = verified_empty_generation(31);
    let first_id = first.metadata().generation();
    let mut generations = GenerationSet::new(1).expect("retention bound is valid");
    generations
        .publish(
            first,
            FakeSearch {
                generation: first_id,
                hits: Vec::new(),
            },
            true,
        )
        .expect("generation publishes");

    let lease = generations
        .lease(first_id)
        .expect("loaded generation leases");
    assert!(
        !generations
            .unload(first_id)
            .expect("pinned unload is checked"),
        "a live query lease must pin its immutable payload"
    );
    assert_eq!(lease.generation().metadata().generation(), first_id);
    drop(lease);

    assert!(
        generations
            .unload(first_id)
            .expect("unpinned payload unloads")
    );
    assert!(generations.contains(first_id));
    assert!(!generations.is_loaded(first_id));
    assert_eq!(
        generations
            .metadata(first_id)
            .expect("metadata remains retained")
            .generation(),
        first_id
    );
    assert!(matches!(
        generations.query(first_id),
        Err(QueryError::GenerationNotFound)
    ));

    let wrong_search_generation = verified_empty_generation(32).metadata().generation();
    assert!(matches!(
        generations.reload(
            verified_empty_generation(31),
            FakeSearch {
                generation: wrong_search_generation,
                hits: Vec::new(),
            },
        ),
        Err(QueryError::GenerationMismatch)
    ));
    assert!(!generations.is_loaded(first_id));

    generations
        .reload(
            verified_empty_generation(31),
            FakeSearch {
                generation: first_id,
                hits: Vec::new(),
            },
        )
        .expect("matching payload reloads");
    assert_eq!(generations.active_generation(), Some(first_id));
    assert!(generations.is_loaded(first_id));
    assert!(generations.lease(first_id).is_ok());
}

#[test]
fn staged_generation_is_hidden_until_commit_and_can_be_discarded() {
    let staged = verified_empty_generation(21);
    let staged_id = staged.metadata().generation();
    let mut generations = GenerationSet::new(2).expect("retention bound is valid");
    generations
        .stage(
            staged,
            FakeSearch {
                generation: staged_id,
                hits: Vec::new(),
            },
        )
        .expect("generation stages");

    assert_eq!(generations.active_generation(), None);
    assert_eq!(generations.len(), 0);
    assert!(matches!(
        generations.query(staged_id),
        Err(QueryError::GenerationNotFound)
    ));
    assert!(matches!(
        generations.generation(staged_id),
        Err(QueryError::GenerationNotFound)
    ));

    generations
        .commit_staged(staged_id, true)
        .expect("staged generation commits");
    assert_eq!(generations.active_generation(), Some(staged_id));
    assert_eq!(generations.len(), 1);
    assert!(generations.query(staged_id).is_ok());

    let discarded = verified_empty_generation(22);
    let discarded_id = discarded.metadata().generation();
    generations
        .stage(
            discarded,
            FakeSearch {
                generation: discarded_id,
                hits: Vec::new(),
            },
        )
        .expect("second generation stages");
    generations
        .discard_staged(discarded_id)
        .expect("staged generation discards");
    assert!(matches!(
        generations.commit_staged(discarded_id, false),
        Err(QueryError::GenerationNotFound)
    ));
    assert_eq!(generations.len(), 1);
}

#[test]
fn rolled_back_commit_restores_the_prior_active_generation() {
    let prior = verified_empty_generation(23);
    let prior_id = prior.metadata().generation();
    let replacement = verified_empty_generation(24);
    let replacement_id = replacement.metadata().generation();
    let mut generations = GenerationSet::new(2).expect("retention bound is valid");
    generations
        .publish(
            prior,
            FakeSearch {
                generation: prior_id,
                hits: Vec::new(),
            },
            true,
        )
        .expect("prior generation publishes");
    generations
        .stage(
            replacement,
            FakeSearch {
                generation: replacement_id,
                hits: Vec::new(),
            },
        )
        .expect("replacement generation stages");
    generations
        .commit_staged(replacement_id, true)
        .expect("replacement generation commits");

    generations
        .rollback_commit(replacement_id, Some(prior_id))
        .expect("replacement commit rolls back");

    assert_eq!(generations.active_generation(), Some(prior_id));
    assert_eq!(generations.len(), 1);
    assert!(generations.query(prior_id).is_ok());
    assert!(matches!(
        generations.query(replacement_id),
        Err(QueryError::GenerationNotFound)
    ));
    generations
        .discard_staged(replacement_id)
        .expect("rolled-back replacement remains staged");
}

#[test]
fn generation_set_rejects_invalid_mismatched_duplicate_and_excess_state() {
    assert!(matches!(
        GenerationSet::<FakeSearch>::new(0),
        Err(QueryError::InvalidGenerationSet)
    ));

    let mismatched = verified_empty_generation(3);
    let foreign = verified_empty_generation(4).metadata().generation();
    let mut generations = GenerationSet::new(2).expect("retention bound is valid");
    assert!(matches!(
        generations.publish(
            mismatched,
            FakeSearch {
                generation: foreign,
                hits: Vec::new(),
            },
            true,
        ),
        Err(QueryError::GenerationMismatch)
    ));

    let retained = verified_empty_generation(5);
    let retained_id = retained.metadata().generation();
    let mut bounded = GenerationSet::new(1).expect("single-generation retention is valid");
    bounded
        .publish(
            retained,
            FakeSearch {
                generation: retained_id,
                hits: Vec::new(),
            },
            true,
        )
        .expect("first generation is retained");
    let excess = verified_empty_generation(6);
    let excess_id = excess.metadata().generation();
    assert!(matches!(
        bounded.publish(
            excess,
            FakeSearch {
                generation: excess_id,
                hits: Vec::new(),
            },
            false,
        ),
        Err(QueryError::RetentionLimit)
    ));

    let first_duplicate = verified_empty_generation(7);
    let duplicate_id = first_duplicate.metadata().generation();
    let mut duplicate_set = GenerationSet::new(2).expect("duplicate fixture capacity is valid");
    duplicate_set
        .publish(
            first_duplicate,
            FakeSearch {
                generation: duplicate_id,
                hits: Vec::new(),
            },
            true,
        )
        .expect("first identity is retained");
    assert!(matches!(
        duplicate_set.publish(
            verified_empty_generation(7),
            FakeSearch {
                generation: duplicate_id,
                hits: Vec::new(),
            },
            false,
        ),
        Err(QueryError::DuplicateGeneration)
    ));
}

fn verified_empty_generation(seed: u8) -> IdentityVerifiedGeneration {
    let repository = RepositoryId::from_bytes([seed; 16]);
    let configuration_hash = content_hash(&[seed, 1]);
    let manifest_hash = GenerationManifestRecipe::new(repository, configuration_hash, Vec::new())
        .expect("empty manifest recipe is valid")
        .canonical_hash()
        .expect("empty manifest recipe encodes");
    let provider_set_hash = content_hash(&[seed, 2]);
    let generation = derive_generation(GenerationIdentity {
        repository,
        parent: None,
        manifest_hash,
        config_hash: configuration_hash,
        provider_set_hash,
        format_version: generation_format_version(),
    })
    .id();
    let metadata = GenerationMetadata::new(
        repository,
        generation,
        None,
        manifest_hash,
        configuration_hash,
        provider_set_hash,
    )
    .expect("empty generation metadata is valid");
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    IdentityVerifiedGeneration::verify(
        metadata,
        NormalizedIrDocument::empty(repository, generation),
        &IrLimits::default(),
        &ExtensionSupport::default(),
        &context,
    )
    .expect("empty generation is identity verified")
}

fn generation_format_version() -> u32 {
    (u32::from(GENERATION_CONTRACT_VERSION.major()) << 16)
        | u32::from(GENERATION_CONTRACT_VERSION.minor())
}
