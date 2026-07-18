//! End-to-end fixtures for the daemon-independent first query slice.

use std::{fs, path::Path, time::Duration};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{
    GenerationIdentity, RepositoryId, content_hash, derive_fact, derive_generation,
    derive_repository,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ExtensionSupport, FactEvidence, FileRecord, IrDocument,
    IrLimits, NormalizedIrDocument, ProducerIdentity, ProducerKind, ProvenanceRecord, SourceRef,
    SourceSpan, decode_ir_document,
};
use rootlight_query::{
    GenerationSet, LocateMode, PlanKind, QueryBudget, QueryError, QueryResource, QueryResponse,
    QueryService, RepositoryDataTrust, TokenAccountingProfile, project_lexical_documents,
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
use rootlight_vfs::{RelativePath, RepositoryRoot};
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
        _request: &SearchRequest,
        _budget: SearchBudget,
        cancellation: &Cancellation,
    ) -> Result<SearchOutcome, SearchError> {
        cancellation.check()?;
        let materialized_text_bytes = self.hits.iter().try_fold(0_u64, |total, hit| {
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
            hits: self.hits.clone(),
            matched_candidates: u64::try_from(self.hits.len())
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
            symbol_id: entity.id,
            file_id: file.id,
            identifier: entity.display_name.clone(),
            qualified_name: entity.qualified_name.clone(),
            path: file.path.clone(),
            kind: serialized_label(&entity.kind),
            language: entity.language.clone(),
            tier: serialized_label(&entity.tier),
            generated: file.generated,
            relevance_score: 1.0,
        }],
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
        located.data.hits[0].trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
    assert!(located.usage.rows >= 2);
    assert!(located.usage.json_bytes > 0);
    assert_exact_response_accounting(&located);

    let explain = service
        .plan_symbol_explain(search.hits[0].symbol_id, QueryBudget::new())
        .expect("explain plan is admitted");
    let explained = service
        .execute_symbol_explain(&explain, &cancellation)
        .expect("explain query succeeds");
    assert_eq!(explained.data.entity.id, search.hits[0].symbol_id);
    assert_eq!(
        explained.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
    assert!(explained.usage.results >= 2);
    assert!(!explained.data.truncated);
    assert_exact_response_accounting(&explained);
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
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .expect("drift fixture plan is admitted");
    assert!(matches!(
        drift_service.execute_code_locate(&drift_plan, &Cancellation::new()),
        Err(QueryError::IndexDrift)
    ));
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
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .expect("configured query-byte boundary is admitted");
    assert!(matches!(
        service.plan_code_locate(
            configured_overflow,
            LocateMode::Exact,
            1,
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
            hard_budget,
            QueryBudget::new(),
        )
        .expect("hard query-byte boundary is admitted");
    assert!(matches!(
        service.plan_code_locate(
            hard_overflow,
            LocateMode::Exact,
            1,
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
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .expect("bounded locate plan is admitted");
    let located = locate_service
        .execute_code_locate(&locate_plan, &Cancellation::new())
        .expect("bounded locate returns a partial prefix");
    assert!(located.data.truncated);
    assert_eq!(located.data.matched_candidates, 2);
    assert_eq!(
        located.data.limiting_resources,
        vec![QueryResource::Results]
    );

    let explain_service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let explain_plan = explain_service
        .plan_symbol_explain(
            search.hits[0].symbol_id,
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
    assert_eq!(first.data.limiting_resources, vec![QueryResource::Rows]);
    assert_eq!(first.data, second.data);
    assert_eq!(first.usage.rows, 2);
}

#[test]
fn plans_and_execution_enforce_all_query_resource_families() {
    let snapshot = fixture_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).expect("generation inputs agree");
    let symbol = search.hits[0].symbol_id;

    assert!(matches!(
        service.plan_code_locate(
            "fixture".to_owned(),
            LocateMode::Text,
            1,
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
    assert_eq!(result.data.chunks[0].text, "beta\n");
    assert_eq!(result.usage.source_bytes, 5);
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
