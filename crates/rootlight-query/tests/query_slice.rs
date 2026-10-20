//! End-to-end fixtures for the daemon-independent first query slice.

use std::{collections::BTreeSet, fs, path::Path, sync::Arc, time::Duration};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{
    FactId, FileIdentity, GenerationIdentity, RepositoryId, SymbolId, content_hash, derive_fact,
    derive_file, derive_generation, derive_repository,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, Confidence, CoverageStatus, DiagnosticRecord,
    DiagnosticSeverity, EntityKind, EntityRecord, EntityVisibility, EvidenceKind, ExtensionSupport,
    FactEvidence, FileRecord, IrDocument, IrLimits, NormalizedIrDocument, OccurrenceRecord,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, ProducerKind, ProvenanceRecord,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan, VbaCompilation,
    VbaCompilationActivity, VbaCompilationBranch, decode_ir_document, new_vba_compilation_envelope,
};
use rootlight_query::{
    ConditionalActivityBasis, ConditionalActivityReason, ConditionalActivityState,
    ExecutionCompletenessState, GenerationSet, LexicalProjectionBuilder, LocateMode, PlanKind,
    QueryBudget, QueryError, QueryResource, QueryResponse, QueryService, RelationDirection,
    RelationFamily, RepositoryDataTrust, TokenAccountingProfile, project_lexical_documents,
    project_lexical_documents_with_all_sources, project_lexical_documents_with_sources,
};
use rootlight_search::{
    BuildBudget, LexicalSearch, QueryViolation, SearchBudget, SearchError, SearchHit,
    SearchOutcome, SearchRequest,
};
use rootlight_source::{LosslessTextReadOptions, SourceBudget, SourceReadOptions, SourceService};
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
    fallback_fixture_with_bytes(
        path,
        b"fallback_identifier { color: green; }\n".to_vec(),
        "utf-8",
    )
}

fn fallback_fixture_with_bytes(
    path: &str,
    content: Vec<u8>,
    encoding: &str,
) -> (GenerationSnapshot, SourceSnapshot) {
    let base = fixture_snapshot();
    let metadata = base.metadata();
    let mut document = base.document().clone();
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
        encoding: encoding.to_owned(),
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
fn scoped_utf16_projection_finds_words_beyond_the_stored_prefix() {
    let text = format!("heading {} suffix_word λ_value", " ".repeat(40_000));
    for (little_endian, encoding) in [(true, "utf-16le"), (false, "utf-16be")] {
        let bytes = std::iter::once(0xfeff_u16)
            .chain(text.encode_utf16())
            .flat_map(|unit| {
                if little_endian {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect::<Vec<_>>();
        let (snapshot, source) = fallback_fixture_with_bytes("encoded.sourceblob", bytes, encoding);
        let prefix = project_lexical_documents_with_sources(
            &snapshot,
            &[&source],
            BuildBudget::default(),
            &Cancellation::new(),
        )
        .unwrap();
        let file = prefix
            .iter()
            .find(|document| document.file_id == source.file())
            .unwrap();
        assert!(file.source_text.as_ref().unwrap().contains("heading"));
        assert!(!file.source_text.as_ref().unwrap().contains("suffix_word"));
        for query in ["suffix_word", "λ_value"] {
            let scoped = rootlight_query::project_scoped_lexical_documents_for_query(
                &snapshot,
                &source,
                query,
                SearchBudget::default(),
                BuildBudget::default(),
                &Cancellation::new(),
            )
            .unwrap();
            let file = scoped
                .iter()
                .find(|document| document.file_id == source.file())
                .unwrap();
            assert!(file.symbol_id.is_none());
            assert_eq!(file.source_text.as_deref(), Some(query));
            assert!(
                file.source_identifiers
                    .iter()
                    .any(|identifier| identifier == query)
            );
        }
        let cancelled = Cancellation::with_deadline(std::time::Instant::now());
        assert!(
            rootlight_query::project_scoped_lexical_documents_for_query(
                &snapshot,
                &source,
                "suffix_word",
                SearchBudget::default(),
                BuildBudget::default(),
                &cancelled
            )
            .is_err()
        );
    }
}

#[test]
fn scoped_normalized_tokens_preserve_authored_text_and_source_identity() {
    for (authored, canonical) in [
        ("e\u{0301}".repeat(81), "é".repeat(81)),
        ("ſ".repeat(240), "s".repeat(240)),
        ("K".repeat(120), "k".repeat(120)),
        ("\u{1100}\u{1161}".repeat(80), "가".repeat(80)),
        (
            format!("{}TailMarker", "e\u{0301}".repeat(81)),
            format!("{}tailmarker", "é".repeat(81)),
        ),
    ] {
        for encoding in ["utf-8", "utf-16le", "utf-16be"] {
            let bytes = match encoding {
                "utf-8" => authored.as_bytes().to_vec(),
                _ => std::iter::once(0xfeff_u16)
                    .chain(authored.encode_utf16())
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect(),
            };
            let (snapshot, source) =
                fallback_fixture_with_bytes("contraction.sourceblob", bytes, encoding);
            let queries: BTreeSet<String> = [
                canonical.clone(),
                canonical.chars().take(20).collect(),
                canonical.chars().take(83).collect(),
            ]
            .into_iter()
            .collect();
            for query in &queries {
                let documents = rootlight_query::project_scoped_lexical_documents_for_query(
                    &snapshot,
                    &source,
                    query,
                    SearchBudget::default(),
                    BuildBudget::default(),
                    &Cancellation::new(),
                )
                .expect("complete normalized token projects without losing authored spelling");
                assert_eq!(documents.len(), 1);
                assert_eq!(documents[0].source_text.as_deref(), Some(authored.as_str()));
                assert_eq!(documents[0].source_identifiers, vec![canonical.clone()]);
                let index = rootlight_search::LexicalIndex::build_ephemeral(
                    snapshot.metadata().generation(),
                    documents,
                    BuildBudget::default(),
                    &Cancellation::new(),
                )
                .unwrap();
                let service = QueryService::new(&snapshot, &index).unwrap();
                let plan = service
                    .plan_code_locate(
                        query.to_string(),
                        LocateMode::Text,
                        10,
                        0,
                        SearchBudget::default(),
                        QueryBudget::default(),
                    )
                    .unwrap();
                let result = service
                    .execute_code_locate(&plan, &Cancellation::new())
                    .unwrap();
                assert_eq!(result.data.hits.len(), 1);
                assert_eq!(result.data.matched_candidates, 1);
                assert!(!result.data.truncated);
                assert!(result.data.limiting_resources.is_empty());
                assert_eq!(
                    result.data.hits[0].source,
                    snapshot.find_file(source.file()).unwrap().evidence.source
                );
            }
        }
    }
}

fn dense_scoped_text_fixture() -> (GenerationSnapshot, SourceSnapshot, SymbolId) {
    let (base, source) = fallback_fixture_with_bytes(
        "dense.sourceblob",
        b"fn carrier(value: OnlySignature) {}\n".to_vec(),
        "utf-8",
    );
    let mut document = base.document().clone();
    let evidence = document
        .files
        .iter()
        .find(|file| file.id == source.file())
        .unwrap()
        .evidence
        .source
        .clone()
        .unwrap();
    let template = document.entities[0].clone();
    let mut selected = None;
    for ordinal in 0_u32..256 {
        let mut entity = template.clone();
        let mut id = [0xee; 20];
        id[..4].copy_from_slice(&ordinal.to_le_bytes());
        entity.id = SymbolId::from_bytes(id);
        entity.canonical_name = format!("unrelated_{ordinal}");
        entity.display_name = entity.canonical_name.clone();
        entity.qualified_name = entity.canonical_name.clone();
        entity.evidence.source = Some(evidence.clone());
        if ordinal == 255 {
            let signature = rootlight_ir::LexicalEvidenceV1::from_complete_text(
                rootlight_ir::LexicalEvidenceKind::Signature,
                rootlight_ir::FactRef::Entity(entity.id),
                rootlight_ir::LexicalEvidenceFormat::SourceText,
                "fn carrier(value: OnlySignature)",
            )
            .unwrap();
            document.extensions.push(
                rootlight_ir::new_lexical_evidence_envelope(
                    document.repository,
                    document.generation,
                    entity.provenance,
                    evidence.clone(),
                    &signature,
                )
                .unwrap(),
            );
            selected = Some(entity.id);
        }
        document.entities.push(entity);
    }
    let snapshot = GenerationSnapshot::new(
        base.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    (snapshot, source, selected.unwrap())
}

#[test]
fn scoped_text_projection_materializes_only_possible_symbol_hits() {
    let (snapshot, source, selected) = dense_scoped_text_fixture();
    for (query, expected_symbol, maximum_documents) in [
        ("only_signature", Some(selected), 2),
        ("()", None, 1),
        ("NeverAppears", None, 1),
    ] {
        let documents = rootlight_query::project_scoped_lexical_documents_for_query(
            &snapshot,
            &source,
            query,
            SearchBudget::default(),
            BuildBudget {
                max_documents: maximum_documents,
                ..BuildBudget::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(documents.len(), maximum_documents);
        assert_eq!(
            documents
                .iter()
                .filter_map(|document| document.symbol_id)
                .collect::<Vec<_>>(),
            expected_symbol.into_iter().collect::<Vec<_>>()
        );
        assert!(
            documents
                .iter()
                .all(|document| document.file_id == source.file())
        );
    }
    for (budget, resource) in [
        (
            BuildBudget {
                max_documents: 1,
                ..BuildBudget::default()
            },
            "documents",
        ),
        (
            BuildBudget {
                max_text_bytes: 1,
                ..BuildBudget::default()
            },
            "text_bytes",
        ),
    ] {
        assert!(matches!(
            rootlight_query::project_scoped_lexical_documents_for_query(
                &snapshot,
                &source,
                "only_signature",
                SearchBudget::default(),
                budget,
                &Cancellation::new(),
            ),
            Err(QueryError::Search(SearchError::BuildBudgetExceeded { resource: actual }))
                if actual == resource
        ));
    }
}

#[test]
fn scoped_text_projection_observation_preserves_documents_and_contains_only_numbers() {
    use rootlight_query::{
        ScopedTextProjectionObservation, project_scoped_lexical_documents_for_query,
        project_scoped_lexical_documents_for_query_with_observation,
    };

    let (snapshot, source, _) = dense_scoped_text_fixture();
    let budget = BuildBudget {
        max_documents: 2,
        ..BuildBudget::default()
    };
    let expected = project_scoped_lexical_documents_for_query(
        &snapshot,
        &source,
        "only_signature",
        SearchBudget::default(),
        budget,
        &Cancellation::new(),
    )
    .unwrap();
    let unobserved = project_scoped_lexical_documents_for_query_with_observation(
        &snapshot,
        &source,
        "only_signature",
        SearchBudget::default(),
        budget,
        &Cancellation::new(),
        None,
    )
    .unwrap();
    let mut observation = ScopedTextProjectionObservation::default();
    let observed = project_scoped_lexical_documents_for_query_with_observation(
        &snapshot,
        &source,
        "only_signature",
        SearchBudget::default(),
        budget,
        &Cancellation::new(),
        Some(&mut observation),
    )
    .unwrap();
    assert_eq!(unobserved, expected);
    assert_eq!(observed, expected);
    assert!(observation.complete);
    assert_eq!(observation.observed_entities, 256);
    assert_eq!(
        observation.observed_envelopes,
        snapshot.document().extensions.len()
    );
    assert_eq!(observation.visited_signature_workspaces, 1);
    assert_eq!(observation.visited_signatures, 1);
    assert_eq!(observation.candidate_entities, 256);
    assert_eq!(observation.retained_entities, 1);
    assert_eq!(observation.projected_documents, expected.len());
    let serialized = serde_json::to_value(&observation).unwrap();
    assert!(
        serialized
            .as_object()
            .unwrap()
            .values()
            .all(|value| value.is_number() || value.is_boolean())
    );
}

#[test]
fn scoped_text_projection_observation_retains_failed_work_and_resets_on_reuse() {
    use rootlight_query::{
        ScopedTextProjectionObservation, project_scoped_lexical_documents_for_query,
        project_scoped_lexical_documents_for_query_with_observation,
    };

    let (snapshot, source, _) = dense_scoped_text_fixture();
    let budget = BuildBudget {
        max_documents: 2,
        ..BuildBudget::default()
    };
    let expected_error = project_scoped_lexical_documents_for_query(
        &snapshot,
        &source,
        "unrelated",
        SearchBudget::default(),
        budget,
        &Cancellation::new(),
    )
    .unwrap_err();
    let mut observation = ScopedTextProjectionObservation::default();
    let actual_error = project_scoped_lexical_documents_for_query_with_observation(
        &snapshot,
        &source,
        "unrelated",
        SearchBudget::default(),
        budget,
        &Cancellation::new(),
        Some(&mut observation),
    )
    .unwrap_err();
    for error in [expected_error, actual_error] {
        assert!(matches!(
            error,
            QueryError::Search(SearchError::BuildBudgetExceeded {
                resource: "documents"
            })
        ));
    }
    assert!(!observation.complete);
    assert_eq!(observation.observed_entities, 256);
    assert_eq!(
        observation.observed_envelopes,
        snapshot.document().extensions.len()
    );
    assert_eq!(observation.candidate_entities, 2);
    assert_eq!(observation.retained_entities, 1);
    assert_eq!(observation.projected_documents, 1);

    let documents = project_scoped_lexical_documents_for_query_with_observation(
        &snapshot,
        &source,
        "()",
        SearchBudget::default(),
        budget,
        &Cancellation::new(),
        Some(&mut observation),
    )
    .unwrap();
    assert!(observation.complete);
    assert_eq!(observation.observed_entities, 0);
    assert_eq!(observation.candidate_entities, 0);
    assert_eq!(observation.visited_signatures, 0);
    assert_eq!(observation.visited_signature_workspaces, 0);
    assert_eq!(observation.retained_entities, 0);
    assert_eq!(observation.projected_documents, 1);
    assert_eq!(documents.len(), 1);

    assert!(
        project_scoped_lexical_documents_for_query_with_observation(
            &snapshot,
            &source,
            "",
            SearchBudget::default(),
            budget,
            &Cancellation::new(),
            Some(&mut observation),
        )
        .is_err()
    );
    assert_eq!(observation, ScopedTextProjectionObservation::default());
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

const CONDITIONAL_SOURCE: &[u8] = b"Public Sub Always()\n#If True Then\n    value = 1\n#Else\n    value = 2\n#End If\nEnd Sub\n#If True Then\nPublic Sub Included()\nEnd Sub\n#Else\nPublic Sub Excluded()\nEnd Sub\n#End If\n#If Host Then\nPublic Sub Unknown()\nEnd Sub\n#End If\n";

fn conditional_fixture() -> (GenerationSnapshot, Arc<SourceSnapshot>, FakeSearch) {
    let template = fixture_snapshot();
    let repository = template.metadata().repository();
    let generation = template.metadata().generation();
    let path = RelativePath::parse(Path::new("Module.bas")).unwrap();
    let file = derive_file(FileIdentity {
        repository,
        path_identity: path.identity_bytes(),
    })
    .id();
    let hash = content_hash(CONDITIONAL_SOURCE);
    let source = |start, end| {
        SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, start, end).unwrap(),
            hash,
            None,
        )
    };
    let full = source(0, u64::try_from(CONDITIONAL_SOURCE.len()).unwrap());
    let parser = derive_fact("rootlight.query-test.conditional-parser/v1", b"module").id();
    let mut document = NormalizedIrDocument::empty(repository, generation);
    document.files.push(FileRecord {
        id: file,
        repository,
        generation,
        path: path.as_str().to_owned(),
        path_locator: Some(path.to_locator()),
        content_hash: hash,
        byte_length: u64::try_from(CONDITIONAL_SOURCE.len()).unwrap(),
        language: "vba".to_owned(),
        encoding: "utf-8".to_owned(),
        generated: false,
        provenance: parser,
        evidence: FactEvidence {
            source: Some(full.clone()),
            derivation: Vec::new(),
        },
    });
    document.provenance.push(ProvenanceRecord {
        id: parser,
        repository,
        generation,
        producer_kind: ProducerKind::Parser,
        producer: ProducerIdentity::new("rootlight-query-test", "1.0.0", hash).unwrap(),
        binary_digest: hash,
        frontend_version: Some("1.0.0".to_owned()),
        language: "vba".to_owned(),
        tier: AnalysisTier::TierB,
        build_context: BuildContextIdentity::new(hash),
        input_sources: vec![full.clone()],
        evidence_sources: vec![full.clone()],
        derivation_parents: Vec::new(),
        rule: None,
    });
    let text = std::str::from_utf8(CONDITIONAL_SOURCE).unwrap();
    for (index, name) in ["Always", "Included", "Excluded", "Unknown"]
        .into_iter()
        .enumerate()
    {
        let token_start = text.find(name).unwrap();
        let token_end = token_start + name.len();
        let body_start = text[..token_start].rfind("Public Sub ").unwrap();
        let body_end = token_end + text[token_end..].find("End Sub\n").unwrap() + "End Sub\n".len();
        let symbol = SymbolId::from_bytes([u8::try_from(index + 1).unwrap(); 20]);
        let token = source(
            u64::try_from(token_start).unwrap(),
            u64::try_from(token_end).unwrap(),
        );
        document.entities.push(EntityRecord {
            id: symbol,
            repository,
            generation,
            kind: EntityKind::Function,
            language: "vba".to_owned(),
            tier: AnalysisTier::TierB,
            canonical_name: name.to_owned(),
            display_name: name.to_owned(),
            qualified_name: format!("Module.{name}"),
            container: None,
            visibility: EntityVisibility::Public,
            flags: Vec::new(),
            provenance: parser,
            evidence: FactEvidence {
                source: Some(source(
                    u64::try_from(body_start).unwrap(),
                    u64::try_from(body_end).unwrap(),
                )),
                derivation: Vec::new(),
            },
        });
        document.occurrences.push(OccurrenceRecord {
            id: derive_fact("rootlight.query-test.definition/v1", name.as_bytes()).id(),
            repository,
            generation,
            file,
            source: token.clone(),
            role: OccurrenceRole::Definition,
            enclosing: None,
            target: OccurrenceTarget::Resolved { symbol },
            syntactic_text_hash: content_hash(name.as_bytes()),
            syntax_kind: "procedure_name".to_owned(),
            provenance: parser,
            confidence: Confidence::new(1000).unwrap(),
            evidence: FactEvidence {
                source: Some(token),
                derivation: Vec::new(),
            },
        });
    }
    let mut branches = Vec::new();
    for (start, _) in text.match_indices("#If ") {
        let header_end = start + text[start..].find('\n').unwrap() + 1;
        let closing = header_end + text[header_end..].find("#End If\n").unwrap();
        let group = source(
            u64::try_from(start).unwrap(),
            u64::try_from(closing + "#End If\n".len()).unwrap(),
        )
        .span();
        if let Some(relative_else) = text[header_end..closing].find("#Else\n") {
            let alternative = header_end + relative_else;
            branches.push(VbaCompilationBranch {
                group,
                region: source(
                    u64::try_from(header_end).unwrap(),
                    u64::try_from(alternative).unwrap(),
                )
                .span(),
                activity: VbaCompilationActivity::Active,
            });
            branches.push(VbaCompilationBranch {
                group,
                region: source(
                    u64::try_from(alternative + "#Else\n".len()).unwrap(),
                    u64::try_from(closing).unwrap(),
                )
                .span(),
                activity: VbaCompilationActivity::Inactive,
            });
        } else {
            branches.push(VbaCompilationBranch {
                group,
                region: source(
                    u64::try_from(header_end).unwrap(),
                    u64::try_from(closing).unwrap(),
                )
                .span(),
                activity: VbaCompilationActivity::Unproven,
            });
        }
    }
    document.extensions.push(
        new_vba_compilation_envelope(
            repository,
            generation,
            parser,
            full.clone(),
            VbaCompilation {
                scope: full.span(),
                complete: true,
                branches,
            },
        )
        .unwrap(),
    );
    let snapshot = GenerationSnapshot::new(
        template.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let search = FakeSearch {
        generation,
        hits: snapshot
            .document()
            .entities
            .iter()
            .map(|entity| SearchHit {
                symbol_id: Some(entity.id),
                file_id: file,
                identifier: entity.display_name.clone(),
                canonical_name: None,
                qualified_name: entity.qualified_name.clone(),
                path: path.as_str().to_owned(),
                kind: serialized_label(&entity.kind),
                language: entity.language.clone(),
                tier: serialized_label(&entity.tier),
                generated: false,
                test: false,
                declaration_only: false,
                relevance_score: 1.0,
            })
            .collect(),
    };
    let retained = Arc::new(
        SourceSnapshot::from_persisted(repository, path, file, hash, CONDITIONAL_SOURCE.to_vec())
            .unwrap(),
    );
    (snapshot, retained, search)
}

fn conditional_locate(
    snapshot: &GenerationSnapshot,
    search: &FakeSearch,
    budget: QueryBudget,
) -> rootlight_query::CodeLocateResult {
    let query = QueryService::new(snapshot, search).unwrap();
    let plan = query
        .plan_code_locate(
            "procedure".to_owned(),
            LocateMode::Exact,
            4,
            0,
            SearchBudget {
                max_candidates: search.hits.len().max(4),
                ..SearchBudget::default()
            },
            budget,
        )
        .unwrap();
    let response = query
        .execute_code_locate(&plan, &Cancellation::new())
        .unwrap();
    assert_exact_response_accounting(&response);
    response.data
}

#[test]
fn conditional_activity_locate_uses_unique_definition_tokens_not_whole_bodies() {
    let (snapshot, _, search) = conditional_fixture();
    let result = conditional_locate(&snapshot, &search, QueryBudget::new());
    assert_eq!(result.hits.len(), 4);
    assert_eq!(result.conditional_activity.len(), 4);
    assert!(!result.execution.is_truncated());
    for assessment in &result.conditional_activity {
        let hit = &result.hits[usize::try_from(assessment.result_index).unwrap()];
        assert_eq!(assessment.basis, ConditionalActivityBasis::DefinitionToken);
        let exact = assessment.assessed_source_ref.as_ref().unwrap();
        let bytes = &CONDITIONAL_SOURCE[usize::try_from(exact.span().start_byte()).unwrap()
            ..usize::try_from(exact.span().end_byte()).unwrap()];
        assert_eq!(bytes, hit.identifier.as_bytes());
        assert_ne!(exact.span(), hit.source.as_ref().unwrap().span());
        let expected = match hit.identifier.as_str() {
            "Always" | "Included" => ConditionalActivityState::Active,
            "Excluded" => ConditionalActivityState::Inactive,
            "Unknown" => ConditionalActivityState::Unproven,
            other => panic!("unexpected authored procedure {other}"),
        };
        assert_eq!(assessment.activity, expected);
        assert_eq!(
            assessment.reason,
            (expected == ConditionalActivityState::Unproven)
                .then_some(ConditionalActivityReason::MembershipUnproven)
        );
        let inventory = assessment.inventory.as_ref().unwrap();
        assert!(inventory.complete);
        assert_eq!(inventory.fact_id, snapshot.document().extensions[0].id);
        assert_eq!(
            inventory.parser_provenance_id,
            snapshot.document().extensions[0].provenance
        );
    }
}

#[test]
fn conditional_activity_missing_ambiguous_and_incomplete_never_guess_a_definition() {
    let (snapshot, _, search) = conditional_fixture();
    for mode in ["missing", "ambiguous", "no_inventory", "incomplete"] {
        let mut document = snapshot.document().clone();
        if mode == "missing" {
            document.occurrences.clear();
        }
        if mode == "ambiguous" {
            let extra: Vec<_> = document
                .occurrences
                .iter()
                .map(|original| {
                    let mut duplicate = original.clone();
                    duplicate.id = derive_fact(
                        "rootlight.query-test.duplicate-definition/v1",
                        original.id.to_string().as_bytes(),
                    )
                    .id();
                    duplicate
                })
                .collect();
            document.occurrences.extend(extra);
        }
        if mode == "no_inventory" {
            document.extensions.clear();
        }
        if mode == "incomplete" {
            let envelope = &document.extensions[0];
            let mut model = rootlight_ir::decode_vba_compilation_envelope(envelope).unwrap();
            model.complete = false;
            document.extensions[0] = new_vba_compilation_envelope(
                document.repository,
                document.generation,
                envelope.provenance,
                envelope.evidence.source.clone().unwrap(),
                model,
            )
            .unwrap();
        }
        let changed = GenerationSnapshot::new(
            snapshot.metadata(),
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        let result = conditional_locate(&changed, &search, QueryBudget::new());
        assert_eq!(result.hits.len(), 4);
        for assessment in result.conditional_activity {
            assert_eq!(
                assessment.activity,
                ConditionalActivityState::Unproven,
                "{mode}"
            );
            assert_eq!(
                assessment.reason,
                Some(match mode {
                    "missing" => ConditionalActivityReason::MissingDefinition,
                    "ambiguous" => ConditionalActivityReason::AmbiguousDefinition,
                    "no_inventory" => ConditionalActivityReason::NoInventory,
                    "incomplete" => ConditionalActivityReason::IncompleteInventory,
                    _ => unreachable!(),
                })
            );
            assert_eq!(
                assessment.assessed_source_ref.is_none(),
                matches!(mode, "missing" | "ambiguous")
            );
            assert_eq!(assessment.inventory.is_some(), mode == "incomplete");
        }
    }
}

#[test]
fn conditional_activity_grouped_generic_definitions_preserve_locate_without_guessing_activity() {
    let template = fixture_snapshot();
    let repository = template.metadata().repository();
    let generation = template.metadata().generation();
    let text = "Item = 7\nItem = 8\n";
    let hash = content_hash(text.as_bytes());
    let path = RelativePath::parse(Path::new("Grouped.bas")).unwrap();
    let file = derive_file(FileIdentity {
        repository,
        path_identity: path.identity_bytes(),
    })
    .id();
    let parser = derive_fact("rootlight.query-test.grouped-parser/v1", b"generic").id();
    let symbol = SymbolId::from_bytes([73; 20]);
    let reference = |start, end| {
        SourceRef::new(
            repository,
            generation,
            SourceSpan::new(
                file,
                u64::try_from(start).unwrap(),
                u64::try_from(end).unwrap(),
            )
            .unwrap(),
            hash,
            None,
        )
    };
    let full = reference(0, text.len());
    let mut document = NormalizedIrDocument::empty(repository, generation);
    document.files.push(FileRecord {
        id: file,
        repository,
        generation,
        path: path.as_str().to_owned(),
        path_locator: Some(path.to_locator()),
        content_hash: hash,
        byte_length: u64::try_from(text.len()).unwrap(),
        language: "vba".to_owned(),
        encoding: "utf-8".to_owned(),
        generated: false,
        provenance: parser,
        evidence: FactEvidence {
            source: Some(full.clone()),
            derivation: Vec::new(),
        },
    });
    document.provenance.push(ProvenanceRecord {
        id: parser,
        repository,
        generation,
        producer_kind: ProducerKind::Parser,
        producer: ProducerIdentity::new("rootlight-query-test", "1.0.0", hash).unwrap(),
        binary_digest: hash,
        frontend_version: None,
        language: "vba".to_owned(),
        tier: AnalysisTier::TierB,
        build_context: BuildContextIdentity::new(hash),
        input_sources: vec![full.clone()],
        evidence_sources: vec![full.clone()],
        derivation_parents: Vec::new(),
        rule: None,
    });
    document.entities.push(EntityRecord {
        id: symbol,
        repository,
        generation,
        kind: EntityKind::Variable,
        language: "vba".to_owned(),
        tier: AnalysisTier::TierB,
        canonical_name: "Item".to_owned(),
        display_name: "Item".to_owned(),
        qualified_name: "Grouped.Item".to_owned(),
        container: Some(rootlight_ir::ContainerRef::File(file)),
        visibility: EntityVisibility::Unknown,
        flags: Vec::new(),
        provenance: parser,
        evidence: FactEvidence {
            source: Some(reference(0, 8)),
            derivation: Vec::new(),
        },
    });
    for (ordinal, start) in [(0_u8, 0), (1, 9)] {
        let token = reference(start, start + "Item".len());
        document.occurrences.push(OccurrenceRecord {
            id: FactId::from_bytes([80 + ordinal; 20]),
            repository,
            generation,
            file,
            source: token.clone(),
            role: OccurrenceRole::Definition,
            enclosing: None,
            target: OccurrenceTarget::Resolved { symbol },
            syntactic_text_hash: content_hash(b"Item"),
            syntax_kind: "generic_name.definition".to_owned(),
            provenance: parser,
            confidence: Confidence::new(1000).unwrap(),
            evidence: FactEvidence {
                source: Some(reference(start, start + 8)),
                derivation: Vec::new(),
            },
        });
        document.relations.push(RelationRecord {
            id: FactId::from_bytes([90 + ordinal; 20]),
            repository,
            generation,
            subject: RelationEndpoint::File(file),
            predicate: RelationPredicate::Contains,
            object: RelationEndpoint::Entity(symbol),
            confidence: Confidence::new(1000).unwrap(),
            evidence_kind: EvidenceKind::Syntax,
            provenance: parser,
            evidence: FactEvidence {
                source: Some(reference(start, start + 8)),
                derivation: Vec::new(),
            },
        });
    }
    document.extensions.push(
        new_vba_compilation_envelope(
            repository,
            generation,
            parser,
            full.clone(),
            VbaCompilation {
                scope: full.span(),
                complete: true,
                branches: Vec::new(),
            },
        )
        .unwrap(),
    );
    let search = FakeSearch {
        generation,
        hits: vec![SearchHit {
            symbol_id: Some(symbol),
            file_id: file,
            identifier: "Item".to_owned(),
            canonical_name: None,
            qualified_name: "Grouped.Item".to_owned(),
            path: path.as_str().to_owned(),
            kind: "variable".to_owned(),
            language: "vba".to_owned(),
            tier: "tier_b".to_owned(),
            generated: false,
            test: false,
            declaration_only: false,
            relevance_score: 1.0,
        }],
    };
    let snapshot = GenerationSnapshot::new(
        template.metadata(),
        document.clone(),
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let result = conditional_locate(&snapshot, &search, QueryBudget::new());
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].symbol, Some(symbol));
    assert_eq!(result.hits[0].source, Some(reference(0, 8)));
    assert!(!result.execution.is_truncated());
    assert_eq!(result.conditional_activity.len(), 1);
    let assessment = &result.conditional_activity[0];
    assert_eq!(assessment.result_index, 0);
    assert_eq!(assessment.activity, ConditionalActivityState::Unproven);
    assert_eq!(
        assessment.reason,
        Some(ConditionalActivityReason::AmbiguousDefinition)
    );
    assert!(assessment.assessed_source_ref.is_none());
    assert!(assessment.inventory.is_none());
    // A second name token alone cannot authorize an arbitrary alternate clause.
    document.relations.pop();
    let unsupported = GenerationSnapshot::new(
        template.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let query = QueryService::new(&unsupported, &search).unwrap();
    let plan = query
        .plan_code_locate(
            "Item".to_owned(),
            LocateMode::Exact,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .unwrap();
    assert!(matches!(
        query.execute_code_locate(&plan, &Cancellation::new()),
        Err(QueryError::IndexDrift)
    ));
}

#[test]
fn conditional_activity_source_reads_assess_expanded_bytes_and_keep_inactive_source() {
    let (snapshot, retained, search) = conditional_fixture();
    let sources = [retained];
    let source = SourceService::from_snapshots(&sources, &snapshot).unwrap();
    let query = QueryService::new(&snapshot, &search).unwrap();
    let locate = conditional_locate(&snapshot, &search, QueryBudget::new());
    let always = locate
        .conditional_activity
        .iter()
        .find(|assessment| {
            locate.hits[usize::try_from(assessment.result_index).unwrap()].identifier == "Always"
        })
        .unwrap();
    assert_eq!(always.activity, ConditionalActivityState::Active);
    let token = always.assessed_source_ref.clone().unwrap();
    for (options, expected) in [
        (
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            ConditionalActivityState::Active,
        ),
        (
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(8),
            ConditionalActivityState::Unproven,
        ),
    ] {
        let plan = query
            .plan_source_read(
                vec![token.clone()],
                options,
                SourceBudget::new(),
                QueryBudget::new(),
            )
            .unwrap();
        assert_eq!(
            plan.explanation().estimate.rows,
            QueryBudget::new().max_rows()
        );
        assert_eq!(
            plan.explanation().estimate.memory_bytes,
            QueryBudget::new().max_memory_bytes()
        );
        let response = query
            .execute_source_read(&plan, &source, &Cancellation::new())
            .unwrap();
        assert_exact_response_accounting(&response);
        let chunk = &response.data.chunks[0];
        let assessment = &response.data.conditional_activity[0];
        assert_eq!(assessment.basis, ConditionalActivityBasis::ReturnedRange);
        assert_eq!(assessment.activity, expected);
        let assessed = assessment.assessed_source_ref.as_ref().unwrap();
        assert_eq!(
            (assessed.span().start_byte(), assessed.span().end_byte()),
            (chunk.start_byte, chunk.end_byte)
        );
        assert_eq!(
            chunk.bytes,
            CONDITIONAL_SOURCE[usize::try_from(chunk.start_byte).unwrap()
                ..usize::try_from(chunk.end_byte).unwrap()]
        );
    }
    let excluded = locate
        .hits
        .iter()
        .find(|hit| hit.identifier == "Excluded")
        .unwrap()
        .source
        .clone()
        .unwrap();
    let plan = query
        .plan_source_read(
            vec![excluded],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            SourceBudget::new(),
            QueryBudget::new(),
        )
        .unwrap();
    let response = query
        .execute_source_read(&plan, &source, &Cancellation::new())
        .unwrap();
    assert_eq!(
        response.data.conditional_activity[0].activity,
        ConditionalActivityState::Inactive
    );
    assert!(
        std::str::from_utf8(&response.data.chunks[0].bytes)
            .unwrap()
            .contains("Public Sub Excluded()")
    );
}

#[test]
fn conditional_activity_resource_stop_preserves_records_without_membership_or_continuation() {
    let (snapshot, retained, mut search) = conditional_fixture();
    let complete = conditional_locate(&snapshot, &search, QueryBudget::new());
    search.hits.extend(search.hits.clone());
    let stopped = conditional_locate(&snapshot, &search, QueryBudget::new().with_max_rows(12));
    assert_eq!(stopped.hits, complete.hits);
    assert!(stopped.execution.is_truncated());
    assert_eq!(stopped.next_page_offset, None);
    assert!(stopped.limiting_resources.contains(&QueryResource::Rows));
    for assessment in stopped.conditional_activity {
        assert_eq!(assessment.activity, ConditionalActivityState::Unproven);
        assert_eq!(
            assessment.reason,
            Some(ConditionalActivityReason::ScanIncomplete)
        );
        assert!(assessment.assessed_source_ref.is_none());
        assert!(assessment.inventory.is_none());
    }
    let sources = [retained];
    let source = SourceService::from_snapshots(&sources, &snapshot).unwrap();
    let query = QueryService::new(&snapshot, &search).unwrap();
    let reference = complete.hits[0].source.clone().unwrap();
    let options = SourceReadOptions::new()
        .with_context_lines_before(0)
        .with_context_lines_after(0);
    let full_plan = query
        .plan_source_read(
            vec![reference.clone()],
            options,
            SourceBudget::new(),
            QueryBudget::new(),
        )
        .unwrap();
    let full = query
        .execute_source_read(&full_plan, &source, &Cancellation::new())
        .unwrap();
    let limited_plan = query
        .plan_source_read(
            vec![reference],
            options,
            SourceBudget::new(),
            QueryBudget::new().with_max_rows(1),
        )
        .unwrap();
    let limited = query
        .execute_source_read(&limited_plan, &source, &Cancellation::new())
        .unwrap();
    assert_eq!(limited.data.chunks, full.data.chunks);
    assert!(limited.data.execution.is_truncated());
    assert_eq!(
        limited.data.conditional_activity[0].reason,
        Some(ConditionalActivityReason::ScanIncomplete)
    );
    assert!(
        limited.data.conditional_activity[0]
            .assessed_source_ref
            .is_some()
    );
    assert!(limited.data.conditional_activity[0].inventory.is_none());
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        query.execute_source_read(&full_plan, &source, &cancellation),
        Err(QueryError::Cancelled(CancellationReason::ClientRequest))
    ));
}

#[test]
fn lossless_text_preserves_conditional_assessed_physical_ranges() {
    let (snapshot, retained, search) = conditional_fixture();
    let sources = [retained];
    let source = SourceService::from_snapshots(&sources, &snapshot).unwrap();
    let query = QueryService::new(&snapshot, &search).unwrap();
    let located = conditional_locate(&snapshot, &search, QueryBudget::new());
    let reference = located
        .hits
        .iter()
        .find(|hit| hit.identifier == "Excluded")
        .unwrap()
        .source
        .clone()
        .unwrap();
    let plan = query
        .plan_source_read_lossless_text(
            vec![reference.clone(), reference.clone()],
            LosslessTextReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            SourceBudget::new(),
            QueryBudget::new(),
        )
        .unwrap();
    let response = query
        .execute_source_read_lossless_text(&plan, &source, &Cancellation::new())
        .unwrap();
    assert_eq!(response.data.chunks.len(), 2);
    assert_eq!(response.data.conditional_activity.len(), 2);
    for (index, chunk) in response.data.chunks.iter().enumerate() {
        assert_eq!(chunk.raw().reference, reference);
        assert_eq!(chunk.text().as_bytes(), chunk.raw().bytes);
        assert_eq!(chunk.physical_encoding(), rootlight_vfs::TextEncoding::Utf8);
        let assessment = &response.data.conditional_activity[index];
        assert_eq!(assessment.result_index, u32::try_from(index).unwrap());
        assert_eq!(assessment.activity, ConditionalActivityState::Inactive);
        let assessed = assessment.assessed_source_ref.as_ref().unwrap();
        assert_eq!(assessed.span().start_byte(), chunk.raw().start_byte);
        assert_eq!(assessed.span().end_byte(), chunk.raw().end_byte);
    }
    assert_eq!(
        response.data.total_source_bytes,
        response.data.total_text_bytes
    );
    assert_eq!(
        response.usage.source_bytes,
        u64::try_from(response.data.total_source_bytes).unwrap()
    );
    assert_exact_response_accounting(&response);
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        query.execute_source_read_lossless_text(&plan, &source, &cancellation),
        Err(QueryError::Cancelled(CancellationReason::ClientRequest))
    ));
}

#[test]
fn lossless_text_separates_utf16_physical_and_presentation_costs() {
    let (template, _, _) = conditional_fixture();
    let text = "\u{feff}a\u{010a}\u{0a00}😀\r\nnext\r\n";
    for encoding in [
        rootlight_vfs::TextEncoding::Utf16Le,
        rootlight_vfs::TextEncoding::Utf16Be,
    ] {
        let bytes: Vec<u8> = text
            .encode_utf16()
            .flat_map(|unit| match encoding {
                rootlight_vfs::TextEncoding::Utf16Le => unit.to_le_bytes(),
                rootlight_vfs::TextEncoding::Utf16Be => unit.to_be_bytes(),
                rootlight_vfs::TextEncoding::Utf8 => unreachable!("fixture covers UTF-16 only"),
            })
            .collect();
        let mut file = template.document().files[0].clone();
        file.language = "unknown".to_owned();
        file.encoding = encoding.as_str().to_owned();
        file.content_hash = content_hash(&bytes);
        file.byte_length = u64::try_from(bytes.len()).unwrap();
        let reference = SourceRef::new(
            file.repository,
            file.generation,
            SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
            file.content_hash,
            None,
        );
        file.evidence.source = Some(reference.clone());
        let path = RelativePath::parse(Path::new(&file.path)).unwrap();
        let retained = Arc::new(
            SourceSnapshot::from_persisted(
                file.repository,
                path,
                file.id,
                file.content_hash,
                bytes.clone(),
            )
            .unwrap(),
        );
        let mut provenance = template.document().provenance[0].clone();
        provenance.language = file.language.clone();
        provenance.input_sources = vec![reference.clone()];
        provenance.evidence_sources = vec![reference.clone()];
        let mut document = NormalizedIrDocument::empty(file.repository, file.generation);
        document.provenance.push(provenance);
        document.files.push(file);
        let snapshot = GenerationSnapshot::new(
            template.metadata(),
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        let search = FakeSearch {
            generation: snapshot.metadata().generation(),
            hits: Vec::new(),
        };
        let query = QueryService::new(&snapshot, &search).unwrap();
        let sources = [retained];
        let source = SourceService::from_snapshots(&sources, &snapshot).unwrap();
        let options = LosslessTextReadOptions::new()
            .with_context_lines_before(0)
            .with_context_lines_after(0);
        let source_budget = SourceBudget::new().with_max_source_bytes(128);
        let query_budget = QueryBudget::new().with_max_source_bytes(128);
        let plan = query
            .plan_source_read_lossless_text(
                vec![reference.clone(), reference.clone()],
                options,
                source_budget,
                query_budget,
            )
            .unwrap();
        let response = query
            .execute_source_read_lossless_text(&plan, &source, &Cancellation::new())
            .unwrap();
        for chunk in &response.data.chunks {
            assert_eq!(chunk.raw().reference, reference);
            assert_eq!(chunk.raw().bytes, bytes);
            assert_eq!(chunk.text(), text);
            assert_eq!(chunk.physical_encoding(), encoding);
            assert_eq!(chunk.trust(), RepositoryDataTrust::UntrustedRepositoryData);
        }
        assert_eq!(response.data.total_source_bytes, bytes.len() * 2);
        assert_eq!(response.data.total_text_bytes, text.len() * 2);
        assert_ne!(
            response.data.total_source_bytes,
            response.data.total_text_bytes
        );
        assert_eq!(
            response.usage.source_bytes,
            u64::try_from(bytes.len() * 2).unwrap()
        );
        assert_exact_response_accounting(&response);
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["data"]["chunks"][0]["content"], text);
        assert_eq!(
            value["data"]["chunks"][0]["physical_encoding"],
            encoding.as_str()
        );
        assert!(value["data"]["chunks"][0].get("bytes").is_none());
        assert!(value["data"]["chunks"][0].get("presentation").is_none());
        let limited = query
            .plan_source_read_lossless_text(
                vec![reference.clone()],
                options,
                SourceBudget::new().with_max_source_bytes(bytes.len() - 1),
                query_budget,
            )
            .unwrap();
        assert!(matches!(
            query.execute_source_read_lossless_text(&limited, &source, &Cancellation::new()),
            Err(QueryError::Source(
                rootlight_source::SourceError::SourceBudgetExceeded
            ))
        ));
        let malformed = SourceRef::new(
            reference.repository(),
            reference.generation(),
            SourceSpan::new(reference.span().file(), 3, 4).unwrap(),
            reference.content_hash(),
            None,
        );
        let invalid = query
            .plan_source_read_lossless_text(vec![malformed], options, source_budget, query_budget)
            .unwrap();
        assert!(
            query
                .execute_source_read_lossless_text(&invalid, &source, &Cancellation::new())
                .is_err()
        );
    }
}

#[path = "support/source_metadata_quote.rs"]
mod source_metadata_quote;
#[path = "support/source_stream_duration.rs"]
mod source_stream_duration;
#[path = "support/source_text_prefix.rs"]
mod source_text_prefix;

#[test]
fn conditional_activity_branchless_batch_reuses_decoder_scratch_under_default_budget() {
    let (template, _, _) = conditional_fixture();
    let mut document = NormalizedIrDocument::empty(
        template.metadata().repository(),
        template.metadata().generation(),
    );
    let repository = document.repository;
    let generation = document.generation;
    let parser = template.document().provenance[0].id;
    let content = b"Public Sub Plain()\nEnd Sub\n";
    let hash = content_hash(content);
    let mut references = Vec::new();
    let mut sources = Vec::new();
    let mut producer = template.document().provenance[0].clone();
    producer.input_sources.clear();
    producer.evidence_sources.clear();
    for index in 0..32 {
        let path = RelativePath::parse(Path::new(&format!("Module{index}.bas"))).unwrap();
        let file = derive_file(FileIdentity {
            repository,
            path_identity: path.identity_bytes(),
        })
        .id();
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, u64::try_from(content.len()).unwrap()).unwrap(),
            hash,
            None,
        );
        references.push(source.clone());
        producer.input_sources.push(source.clone());
        producer.evidence_sources.push(source.clone());
        document.files.push(FileRecord {
            id: file,
            repository,
            generation,
            path: path.as_str().to_owned(),
            path_locator: Some(path.to_locator()),
            content_hash: hash,
            byte_length: u64::try_from(content.len()).unwrap(),
            language: "visual-basic-classic".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance: parser,
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation: Vec::new(),
            },
        });
        document.extensions.push(
            new_vba_compilation_envelope(
                repository,
                generation,
                parser,
                source.clone(),
                VbaCompilation {
                    scope: source.span(),
                    complete: true,
                    branches: Vec::new(),
                },
            )
            .unwrap(),
        );
        sources.push(Arc::new(
            SourceSnapshot::from_persisted(repository, path, file, hash, content.to_vec()).unwrap(),
        ));
    }
    document.provenance.push(producer);
    let snapshot = GenerationSnapshot::new(
        template.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    sources.sort_by_key(|source| source.file());
    let search = FakeSearch {
        generation,
        hits: Vec::new(),
    };
    let query = QueryService::new(&snapshot, &search).unwrap();
    let source = SourceService::from_snapshots(&sources, &snapshot).unwrap();
    let plan = query
        .plan_source_read(
            references.clone(),
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            SourceBudget::new(),
            QueryBudget::new(),
        )
        .unwrap();
    let response = query
        .execute_source_read(&plan, &source, &Cancellation::new())
        .unwrap();
    assert_exact_response_accounting(&response);
    assert!(!response.data.execution.is_truncated());
    assert_eq!(response.data.chunks.len(), 32);
    assert_eq!(response.data.conditional_activity.len(), 32);
    for (index, assessment) in response.data.conditional_activity.iter().enumerate() {
        assert_eq!(assessment.result_index, u32::try_from(index).unwrap());
        assert_eq!(assessment.activity, ConditionalActivityState::Active);
        assert_eq!(
            assessment.assessed_source_ref,
            Some(references[index].clone())
        );
        assert!(assessment.inventory.as_ref().unwrap().complete);
        assert_eq!(response.data.chunks[index].bytes, content);
    }
    assert!(response.usage.memory_bytes <= plan.explanation().estimate.memory_bytes);
    assert!(response.usage.rows <= plan.explanation().estimate.rows);
}

#[test]
fn conditional_activity_file_only_range_and_disjoint_scopes_are_not_definition_guesses() {
    let (snapshot, _, mut search) = conditional_fixture();
    let file = &snapshot.document().files[0];
    search.hits = vec![SearchHit {
        symbol_id: None,
        file_id: file.id,
        identifier: file.path.clone(),
        canonical_name: None,
        qualified_name: file.path.clone(),
        path: file.path.clone(),
        kind: "file".to_owned(),
        language: file.language.clone(),
        tier: serialized_label(&snapshot.document().provenance[0].tier),
        generated: false,
        test: false,
        declaration_only: false,
        relevance_score: 1.0,
    }];
    let result = conditional_locate(&snapshot, &search, QueryBudget::new());
    assert_eq!(
        result.conditional_activity[0].basis,
        ConditionalActivityBasis::ReturnedRange
    );
    assert_eq!(
        result.conditional_activity[0].activity,
        ConditionalActivityState::Unproven
    );
    assert_eq!(
        result.conditional_activity[0].reason,
        Some(ConditionalActivityReason::MembershipUnproven)
    );
    assert_eq!(
        result.conditional_activity[0].assessed_source_ref,
        result.hits[0].source
    );
    let mut document = snapshot.document().clone();
    let full = document.files[0].evidence.source.as_ref().unwrap();
    let split = u64::try_from(CONDITIONAL_SOURCE.len() / 2).unwrap();
    document.extensions = [(0, split), (split, file.byte_length)]
        .into_iter()
        .map(|(start, end)| {
            let source = SourceRef::new(
                document.repository,
                document.generation,
                SourceSpan::new(file.id, start, end).unwrap(),
                full.content_hash(),
                None,
            );
            new_vba_compilation_envelope(
                document.repository,
                document.generation,
                file.provenance,
                source.clone(),
                VbaCompilation {
                    scope: source.span(),
                    complete: true,
                    branches: Vec::new(),
                },
            )
            .unwrap()
        })
        .collect();
    let divided = GenerationSnapshot::new(
        snapshot.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let result = conditional_locate(&divided, &search, QueryBudget::new());
    assert_eq!(
        result.conditional_activity[0].reason,
        Some(ConditionalActivityReason::CrossScope)
    );
    assert!(result.conditional_activity[0].inventory.is_none());
    assert_eq!(
        result.conditional_activity[0].assessed_source_ref,
        result.hits[0].source
    );
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
fn locate_preserves_long_symbol_names_and_source_identity() {
    let original = fixture_snapshot();
    let mut document = original.document().clone();
    let display = format!("DistinctName_{}_Tail", "x".repeat(520));
    let qualified = format!("Container::{}::{display}", "scope::".repeat(310));
    let entity = &mut document.entities[0];
    entity.display_name = display.clone();
    entity.qualified_name = qualified.clone();
    let expected = entity.clone();
    let snapshot = GenerationSnapshot::new(
        original.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("complete names satisfy the unchanged IR contract");
    let cancellation = Cancellation::new();
    let documents =
        project_lexical_documents(&snapshot, BuildBudget::default(), &cancellation).unwrap();
    let index = rootlight_search::LexicalIndex::build_ephemeral(
        snapshot.metadata().generation(),
        documents,
        BuildBudget::default(),
        &cancellation,
    )
    .expect("valid long symbol names must not prevent generation construction");
    let service = QueryService::new(&snapshot, &index).unwrap();
    for (query, mode) in [
        (expected.canonical_name.as_str(), LocateMode::Exact),
        ("DistinctName", LocateMode::Prefix),
        ("Distinct Name", LocateMode::Text),
        ("Distinct*Tail", LocateMode::Glob),
        ("Distinct.*Tail", LocateMode::SafeRegex),
    ] {
        let plan = service
            .plan_code_locate(
                query.to_owned(),
                mode,
                10,
                0,
                SearchBudget::default(),
                QueryBudget::new(),
            )
            .unwrap();
        let response = service.execute_code_locate(&plan, &cancellation).unwrap();
        let hit = response
            .data
            .hits
            .iter()
            .find(|hit| hit.symbol == Some(expected.id))
            .unwrap_or_else(|| panic!("authoritative symbol hit for {mode:?}: {query}"));
        assert_eq!(hit.identifier, display);
        assert_eq!(hit.qualified_name, qualified);
        assert_eq!(hit.source, expected.evidence.source);
        assert_exact_response_accounting(&response);
    }
    assert_eq!(snapshot.document().entities[0], expected);
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

fn signature_batch_snapshot(direct: Option<&str>) -> (GenerationSnapshot, SymbolId, SymbolId) {
    let (base, first) = signature_snapshot(&["fn first(FirstOnly)"]);
    let mut document = base.document().clone();
    let first_child = document
        .extensions
        .iter()
        .find(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .unwrap()
        .clone();
    let mut neighbor = document
        .entities
        .iter()
        .find(|entity| entity.id == first)
        .unwrap()
        .clone();
    neighbor.id = SymbolId::from_bytes([201; 20]);
    neighbor.language = "common-lisp".to_owned();
    neighbor.canonical_name = "neighbor".to_owned();
    neighbor.display_name = "neighbor".to_owned();
    neighbor.qualified_name = "neighbor".to_owned();
    let second = neighbor.id;
    let lexical = rootlight_ir::LexicalEvidenceV1::from_complete_text(
        rootlight_ir::LexicalEvidenceKind::Signature,
        rootlight_ir::FactRef::Entity(second),
        rootlight_ir::LexicalEvidenceFormat::SourceText,
        "(defun neighbor (NeighborOnly))",
    )
    .unwrap();
    let second_child = rootlight_ir::new_lexical_evidence_envelope(
        document.repository,
        document.generation,
        neighbor.provenance,
        neighbor.evidence.source.clone().unwrap(),
        &lexical,
    )
    .unwrap();
    document.entities.push(neighbor);
    document
        .extensions
        .retain(|extension| extension.namespace != rootlight_ir::LEXICAL_EXTENSION_NAMESPACE);
    document.extensions.push(
        rootlight_ir::new_lexical_signature_batch_envelope(&[first_child.clone(), second_child])
            .unwrap(),
    );
    if let Some(text) = direct {
        let evidence = rootlight_ir::LexicalEvidenceV1::from_complete_text(
            rootlight_ir::LexicalEvidenceKind::Signature,
            rootlight_ir::FactRef::Entity(first),
            rootlight_ir::LexicalEvidenceFormat::SourceText,
            text,
        )
        .unwrap();
        document.extensions.push(
            rootlight_ir::new_lexical_evidence_envelope(
                document.repository,
                document.generation,
                first_child.provenance,
                first_child.evidence.source.unwrap(),
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
        first,
        second,
    )
}

fn sequential_signature_batch_snapshot() -> (GenerationSnapshot, FakeSearch, u64) {
    let base = fixture_snapshot();
    let mut document = base.document().clone();
    let template = document.entities[0].clone();
    let hit_template = fixture_search(&base).hits.remove(0);
    let mut selected = Vec::new();
    let mut maximum_workspace = 0;
    for batch in 0_u16..19 {
        let mut children = Vec::new();
        for child in 0_u16..128 {
            let ordinal = batch * 128 + child;
            let mut identity = [0xa3; 20];
            identity[18..].copy_from_slice(&ordinal.to_be_bytes());
            let mut entity = template.clone();
            entity.id = SymbolId::from_bytes(identity);
            entity.canonical_name = format!("shared_{ordinal}");
            entity.display_name = entity.canonical_name.clone();
            entity.qualified_name = entity.canonical_name.clone();
            let is_selected = child < 6 || (child == 6 && batch < 14);
            let text = if is_selected {
                "fn shared()".to_owned()
            } else {
                format!("fn neighbor({})", "Argument".repeat(25))
            };
            let evidence = rootlight_ir::LexicalEvidenceV1::from_complete_text(
                rootlight_ir::LexicalEvidenceKind::Signature,
                rootlight_ir::FactRef::Entity(entity.id),
                rootlight_ir::LexicalEvidenceFormat::SourceText,
                &text,
            )
            .unwrap();
            children.push(
                rootlight_ir::new_lexical_evidence_envelope(
                    document.repository,
                    document.generation,
                    entity.provenance,
                    entity.evidence.source.clone().unwrap(),
                    &evidence,
                )
                .unwrap(),
            );
            if is_selected {
                let mut hit = hit_template.clone();
                hit.symbol_id = Some(entity.id);
                hit.identifier = entity.display_name.clone();
                hit.canonical_name = None;
                hit.qualified_name = entity.qualified_name.clone();
                selected.push((child, batch, hit));
            }
            document.entities.push(entity);
        }
        let envelope = rootlight_ir::new_lexical_signature_batch_envelope(&children).unwrap();
        let workspace = u64::try_from(
            rootlight_ir::lexical_signature_batch_decode_workspace_bytes(&envelope).unwrap(),
        )
        .unwrap();
        maximum_workspace = maximum_workspace.max(workspace);
        document.extensions.push(envelope);
    }
    // Both 64-hit pages span all nineteen batches, including hidden neighbors.
    selected.sort_unstable_by_key(|(child, batch, _)| (*child, *batch));
    assert_eq!(selected.len(), 128);
    let snapshot = GenerationSnapshot::new(
        base.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let search = FakeSearch {
        generation: snapshot.metadata().generation(),
        hits: selected.into_iter().map(|(_, _, hit)| hit).collect(),
    };
    (snapshot, search, maximum_workspace)
}

#[test]
fn locate_signature_batches_reserve_sequential_scratch_once() {
    let (snapshot, search, maximum_workspace) = sequential_signature_batch_snapshot();
    let service = QueryService::new(&snapshot, &search).unwrap();
    let budget = QueryBudget::new();
    assert!(maximum_workspace < budget.max_memory_bytes());
    assert!(19 * 2 * maximum_workspace > budget.max_memory_bytes());
    let plan = |offset, budget| {
        service.plan_code_locate(
            "shared".to_owned(),
            LocateMode::Prefix,
            64,
            offset,
            SearchBudget::default(),
            budget,
        )
    };
    let mut seen = BTreeSet::new();
    for offset in [0, 64] {
        let response = service
            .execute_code_locate(&plan(offset, budget).unwrap(), &Cancellation::new())
            .unwrap();
        assert_eq!(response.data.hits.len(), 64);
        assert_eq!(response.data.matched_candidates, 128);
        assert_eq!(response.data.next_page_offset, (offset == 0).then_some(64));
        assert!(
            !response
                .data
                .limiting_resources
                .contains(&QueryResource::MemoryBytes)
        );
        for hit in &response.data.hits {
            let symbol = hit.symbol.unwrap();
            assert!(seen.insert(symbol));
            let signature = hit
                .signature
                .as_ref()
                .expect("complete page keeps every header");
            assert_eq!(signature.subject(), rootlight_ir::FactRef::Entity(symbol));
            assert_eq!(signature.text(), "fn shared()");
            assert_eq!(
                hit.source.as_ref(),
                snapshot
                    .document()
                    .entities
                    .iter()
                    .find(|entity| entity.id == symbol)
                    .unwrap()
                    .evidence
                    .source
                    .as_ref()
            );
        }
        assert!(response.usage.memory_bytes < maximum_workspace * 2);
        assert!(response.usage.memory_bytes <= budget.max_memory_bytes());
        assert_exact_response_accounting(&response);
    }
    assert_eq!(seen.len(), 128);

    for (bounded, resource) in [
        (
            budget.with_max_memory_bytes(maximum_workspace / 2),
            QueryResource::MemoryBytes,
        ),
        (budget.with_max_rows(1_000), QueryResource::Rows),
        (budget.with_max_source_bytes(1), QueryResource::SourceBytes),
    ] {
        let bounded_plan = match plan(0, bounded) {
            Ok(plan) => plan,
            Err(QueryError::PlanRejected { resource: rejected }) => {
                assert_eq!(rejected, resource);
                continue;
            }
            Err(error) => panic!("unexpected bounded plan error: {error:?}"),
        };
        match service.execute_code_locate(&bounded_plan, &Cancellation::new()) {
            Ok(response) => {
                assert!(response.data.hits.iter().all(|hit| hit.signature.is_none()));
                assert!(response.data.limiting_resources.contains(&resource));
                assert!(response.data.next_page_offset.is_none());
                assert!(response.usage.memory_bytes <= bounded.max_memory_bytes());
                assert!(response.usage.rows <= bounded.max_rows());
                assert_exact_response_accounting(&response);
            }
            Err(QueryError::BudgetExceeded {
                resource: exceeded, ..
            }) => assert_eq!(exceeded, resource),
            Err(error) => panic!("unexpected bounded batch error: {error:?}"),
        }
    }
    let symbol = search.hits[0].symbol_id.unwrap();
    let explain_budget = budget.with_max_memory_bytes(maximum_workspace * 3 / 2);
    let explanation = service
        .execute_symbol_explain(
            &service.plan_symbol_explain(symbol, explain_budget).unwrap(),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(
        explanation.data.signature.as_ref().unwrap().text(),
        "fn shared()"
    );
    assert!(!explanation.data.truncated);
    assert!(explanation.usage.memory_bytes <= explain_budget.max_memory_bytes());
    assert_exact_response_accounting(&explanation);
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        service.execute_code_locate(&plan(0, budget).unwrap(), &cancellation),
        Err(QueryError::Cancelled(CancellationReason::ClientRequest))
    ));
}

fn selective_locate_signature_snapshot(
    snapshot: &GenerationSnapshot,
    count: u16,
) -> GenerationSnapshot {
    let mut document = snapshot.document().clone();
    let neighbor_id = SymbolId::from_bytes([201; 20]);
    if !document
        .entities
        .iter()
        .any(|entity| entity.id == neighbor_id)
    {
        let mut neighbor = document.entities[0].clone();
        neighbor.id = neighbor_id;
        document.entities.push(neighbor);
    }
    let neighbor = document
        .entities
        .iter_mut()
        .find(|entity| entity.id == neighbor_id)
        .unwrap();
    neighbor.canonical_name = "fixture_neighbor".to_owned();
    neighbor.display_name = "fixture_neighbor".to_owned();
    neighbor.qualified_name = "fixture_neighbor".to_owned();
    let mut unrelated = document.entities[0].clone();
    unrelated.id = SymbolId::from_bytes([200; 20]);
    unrelated.kind = EntityKind::Class;
    unrelated.canonical_name = "unrelated".to_owned();
    unrelated.display_name = "unrelated".to_owned();
    unrelated.qualified_name = "unrelated".to_owned();
    for index in 0..count {
        let evidence = rootlight_ir::LexicalEvidenceV1::from_complete_text(
            rootlight_ir::LexicalEvidenceKind::Signature,
            rootlight_ir::FactRef::Entity(unrelated.id),
            rootlight_ir::LexicalEvidenceFormat::SourceText,
            &format!("class Unrelated{index}"),
        )
        .unwrap();
        document.extensions.push(
            rootlight_ir::new_lexical_evidence_envelope(
                document.repository,
                document.generation,
                unrelated.provenance,
                unrelated.evidence.source.clone().unwrap(),
                &evidence,
            )
            .unwrap(),
        );
    }
    document.entities.push(unrelated);
    GenerationSnapshot::new(
        snapshot.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap()
}

#[test]
fn locate_signature_rows_ignore_unrelated_extensions() {
    let (baseline, symbol) = signature_snapshot(&["fn fixture()"]);
    let mut rows = None;
    for count in [0, 64] {
        let snapshot = selective_locate_signature_snapshot(&baseline, count);
        let cancellation = Cancellation::new();
        let projected =
            project_lexical_documents(&snapshot, BuildBudget::default(), &cancellation).unwrap();
        let search = rootlight_search::LexicalIndex::build_ephemeral(
            snapshot.metadata().generation(),
            projected,
            BuildBudget::default(),
            &cancellation,
        )
        .unwrap();
        let service = QueryService::new(&snapshot, &search).unwrap();
        let budget = QueryBudget::new().with_max_rows(16);
        let plan = service
            .plan_code_locate_with_entity_filters(
                "fixture".to_owned(),
                LocateMode::Prefix,
                vec!["rust".to_owned()],
                vec!["src/lib.rs".to_owned()],
                Some(vec!["file".to_owned(), "function".to_owned()]),
                2,
                0,
                SearchBudget {
                    max_candidates: 2,
                    ..SearchBudget::default()
                },
                budget,
            )
            .unwrap();
        let result = service.execute_code_locate(&plan, &cancellation).unwrap();
        assert_eq!(result.data.hits.len(), 2);
        assert_eq!(result.data.matched_candidates, 2);
        assert!(!result.data.truncated);
        assert!(result.data.next_page_offset.is_none());
        let signature = result
            .data
            .hits
            .iter()
            .find(|hit| hit.symbol == Some(symbol))
            .unwrap()
            .signature
            .as_ref()
            .unwrap();
        assert_eq!(signature.text(), "fn fixture()");
        assert_eq!(signature.subject(), rootlight_ir::FactRef::Entity(symbol));
        assert_eq!(*rows.get_or_insert(result.usage.rows), result.usage.rows);
        assert!(result.usage.rows <= budget.max_rows());
        assert_exact_response_accounting(&result);
    }
}

#[test]
fn locate_signature_batches_share_rows_and_preserve_conflicts() {
    for direct in [None, Some("fn first(ConflictingOnly)")] {
        let (baseline, first, second) = signature_batch_snapshot(direct);
        let snapshot = selective_locate_signature_snapshot(&baseline, 128);
        let cancellation = Cancellation::new();
        let projected =
            project_lexical_documents(&snapshot, BuildBudget::default(), &cancellation).unwrap();
        let search = rootlight_search::LexicalIndex::build_ephemeral(
            snapshot.metadata().generation(),
            projected,
            BuildBudget::default(),
            &cancellation,
        )
        .unwrap();
        let service = QueryService::new(&snapshot, &search).unwrap();
        let plan = |max_rows| {
            service
                .plan_code_locate_with_entity_filters(
                    "fixture".to_owned(),
                    LocateMode::Prefix,
                    vec!["common-lisp".to_owned(), "rust".to_owned()],
                    vec!["src/lib.rs".to_owned()],
                    Some(vec!["function".to_owned()]),
                    2,
                    0,
                    SearchBudget {
                        max_candidates: 2,
                        ..SearchBudget::default()
                    },
                    QueryBudget::new().with_max_rows(max_rows),
                )
                .unwrap()
        };
        let result = service
            .execute_code_locate(&plan(300), &cancellation)
            .unwrap();
        assert_eq!(result.data.hits.len(), 2);
        assert!(!result.data.truncated);
        for (symbol, expected) in [
            (first, direct.is_none().then_some("fn first(FirstOnly)")),
            (second, Some("(defun neighbor (NeighborOnly))")),
        ] {
            let hit = result
                .data
                .hits
                .iter()
                .find(|hit| hit.symbol == Some(symbol))
                .unwrap();
            assert_eq!(
                hit.signature.as_ref().map(|evidence| evidence.text()),
                expected
            );
        }
        assert!(result.usage.rows <= 300);
        assert_exact_response_accounting(&result);
        if direct.is_none() {
            let bounded = service
                .execute_code_locate(&plan(260), &cancellation)
                .unwrap();
            assert!(bounded.data.hits.iter().all(|hit| hit.signature.is_none()));
            assert!(bounded.data.truncated);
            assert!(
                bounded
                    .data
                    .limiting_resources
                    .contains(&QueryResource::Rows)
            );
            assert!(bounded.usage.rows <= 260);
            assert_exact_response_accounting(&bounded);
        }
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            service.execute_code_locate(&plan(300), &cancellation),
            Err(QueryError::Cancelled(CancellationReason::ClientRequest))
        ));
    }
}

#[test]
fn signature_batch_explanations_select_exact_children_and_deduplicate_direct_evidence() {
    for direct in [None, Some("fn first(FirstOnly)")] {
        let (snapshot, first, second) = signature_batch_snapshot(direct);
        let search = fixture_search(&snapshot);
        let service = QueryService::new(&snapshot, &search).unwrap();
        for (symbol, text) in [
            (first, "fn first(FirstOnly)"),
            (second, "(defun neighbor (NeighborOnly))"),
        ] {
            let budget =
                QueryBudget::new().with_max_source_bytes(u64::try_from(text.len()).unwrap());
            let result = service
                .execute_symbol_explain(
                    &service.plan_symbol_explain(symbol, budget).unwrap(),
                    &Cancellation::new(),
                )
                .unwrap();
            let signature = result.data.signature.as_ref().unwrap();
            assert_eq!(signature.subject(), rootlight_ir::FactRef::Entity(symbol));
            assert_eq!(signature.text(), text);
            assert_eq!(
                result.usage.source_bytes,
                u64::try_from(text.len()).unwrap()
            );
            assert_exact_response_accounting(&result);
        }
    }
}

#[test]
fn signature_batch_projection_and_locate_do_not_leak_neighbor_headers() {
    let (snapshot, first, second) = signature_batch_snapshot(None);
    let cancellation = Cancellation::new();
    let projected =
        project_lexical_documents(&snapshot, BuildBudget::default(), &cancellation).unwrap();
    assert_eq!(
        projected
            .iter()
            .find(|document| document.symbol_id == Some(first))
            .unwrap()
            .signature
            .as_deref(),
        Some("fn first(FirstOnly)")
    );
    assert_eq!(
        projected
            .iter()
            .find(|document| document.symbol_id == Some(second))
            .unwrap()
            .signature
            .as_deref(),
        Some("(defun neighbor (NeighborOnly))")
    );
    let search = rootlight_search::LexicalIndex::build_ephemeral(
        snapshot.metadata().generation(),
        projected,
        BuildBudget::default(),
        &cancellation,
    )
    .unwrap();
    let service = QueryService::new(&snapshot, &search).unwrap();
    for (needle, symbol) in [("FirstOnly", first), ("NeighborOnly", second)] {
        let plan = service
            .plan_code_locate(
                needle.to_owned(),
                LocateMode::Text,
                10,
                0,
                SearchBudget::default(),
                QueryBudget::new(),
            )
            .unwrap();
        let result = service.execute_code_locate(&plan, &cancellation).unwrap();
        assert_eq!(result.data.hits.len(), 1);
        assert_eq!(result.data.hits[0].symbol, Some(symbol));
        assert_eq!(
            result.data.hits[0].signature.as_ref().unwrap().subject(),
            rootlight_ir::FactRef::Entity(symbol)
        );
        assert_exact_response_accounting(&result);
    }
}

#[test]
fn signature_projection_keeps_unsigned_entities_and_conflicted_batch_members() {
    let (baseline, first, second) = signature_batch_snapshot(Some("fn first(ConflictingOnly)"));
    let mut document = baseline.document().clone();
    let template = document.entities[0].clone();
    let unsigned = (10..74)
        .map(|index| {
            let mut entity = template.clone();
            let mut identity = [0xa5; 20];
            identity[0] = index;
            entity.id = SymbolId::from_bytes(identity);
            assert_ne!(entity.id, first);
            assert_ne!(entity.id, second);
            entity.canonical_name = format!("unsigned_{index}");
            entity.display_name.clone_from(&entity.canonical_name);
            entity.qualified_name.clone_from(&entity.canonical_name);
            entity
        })
        .collect::<Vec<_>>();
    document.entities.extend(unsigned.iter().cloned());
    let snapshot = GenerationSnapshot::new(
        baseline.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let projected =
        project_lexical_documents(&snapshot, BuildBudget::default(), &Cancellation::new()).unwrap();
    assert_eq!(projected.len(), snapshot.document().entities.len());
    for entity in unsigned {
        let matches = projected
            .iter()
            .filter(|row| row.symbol_id == Some(entity.id))
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].identifier, entity.display_name);
        assert!(matches[0].signature.is_none());
    }
    assert!(
        projected
            .iter()
            .find(|row| row.symbol_id == Some(first))
            .unwrap()
            .signature
            .is_none()
    );
    assert_eq!(
        projected
            .iter()
            .find(|row| row.symbol_id == Some(second))
            .unwrap()
            .signature
            .as_deref(),
        Some("(defun neighbor (NeighborOnly))")
    );
}

#[test]
fn signature_batch_conflicts_and_malformed_hidden_neighbors_fail_closed() {
    let (snapshot, first, second) = signature_batch_snapshot(Some("fn first(ConflictingOnly)"));
    let projected =
        project_lexical_documents(&snapshot, BuildBudget::default(), &Cancellation::new()).unwrap();
    assert!(
        projected
            .iter()
            .find(|document| document.symbol_id == Some(first))
            .unwrap()
            .signature
            .is_none()
    );
    assert!(
        projected
            .iter()
            .find(|document| document.symbol_id == Some(second))
            .unwrap()
            .signature
            .is_some()
    );
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).unwrap();
    assert!(
        service
            .execute_symbol_explain(
                &service
                    .plan_symbol_explain(first, QueryBudget::new())
                    .unwrap(),
                &Cancellation::new(),
            )
            .unwrap()
            .data
            .signature
            .is_none()
    );
    let mut document = snapshot.document().clone();
    let batch = document
        .extensions
        .iter_mut()
        .find(|extension| extension.version == rootlight_ir::LEXICAL_SIGNATURE_BATCH_VERSION)
        .unwrap();
    let mut payload: serde_json::Value = serde_json::from_str(&batch.payload).unwrap();
    let entries = payload["entries"].as_array().unwrap();
    let child_position = |symbol| {
        entries
            .iter()
            .position(|entry| {
                rootlight_ir::decode_lexical_evidence(entry["payload"].as_str().unwrap())
                    .unwrap()
                    .subject()
                    == rootlight_ir::FactRef::Entity(symbol)
            })
            .unwrap()
    };
    let first_position = child_position(first);
    let neighbor_position = child_position(second);
    assert_ne!(first_position, neighbor_position);
    let selected_entry = entries[first_position].clone();
    let selected_evidence =
        rootlight_ir::decode_lexical_evidence(selected_entry["payload"].as_str().unwrap()).unwrap();
    assert_eq!(
        selected_evidence.subject(),
        rootlight_ir::FactRef::Entity(first)
    );
    assert_eq!(selected_evidence.text(), "fn first(FirstOnly)");
    payload["entries"][neighbor_position]["payload"] = serde_json::json!("{}");
    assert_eq!(payload["entries"][first_position], selected_entry);
    batch.payload = serde_json::to_string(&payload).unwrap();
    let invalid = GenerationSnapshot::new(
        snapshot.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    assert!(matches!(
        project_lexical_documents(&invalid, BuildBudget::default(), &Cancellation::new()),
        Err(QueryError::IndexDrift)
    ));
    let search = fixture_search(&invalid);
    let service = QueryService::new(&invalid, &search).unwrap();
    assert!(matches!(
        service.execute_symbol_explain(
            &service
                .plan_symbol_explain(first, QueryBudget::new())
                .unwrap(),
            &Cancellation::new(),
        ),
        Err(QueryError::IndexDrift)
    ));
    assert!(matches!(
        service.execute_code_locate(
            &service
                .plan_code_locate(
                    search.hits[0].identifier.clone(),
                    LocateMode::Exact,
                    1,
                    0,
                    SearchBudget::default(),
                    QueryBudget::new(),
                )
                .unwrap(),
            &Cancellation::new(),
        ),
        Err(QueryError::IndexDrift)
    ));
}

#[test]
fn signature_batch_query_limits_and_cancellation_do_not_publish_partial_headers() {
    let (snapshot, first, _) = signature_batch_snapshot(None);
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).unwrap();
    assert!(matches!(
        service.plan_symbol_explain(first, QueryBudget::new().with_max_results(1)),
        Err(QueryError::PlanRejected {
            resource: QueryResource::Results,
        })
    ));
    for budget in [
        QueryBudget::new().with_max_rows(2),
        QueryBudget::new().with_max_memory_bytes(1_024),
        QueryBudget::new().with_max_source_bytes(1),
        QueryBudget::new().with_max_results(2),
    ] {
        let result = service.execute_symbol_explain(
            &service.plan_symbol_explain(first, budget).unwrap(),
            &Cancellation::new(),
        );
        match result {
            Ok(result) => {
                assert!(result.data.signature.is_none());
                assert!(result.data.truncated);
                assert!(result.usage.rows <= budget.max_rows());
                assert!(result.usage.memory_bytes <= budget.max_memory_bytes());
                assert_exact_response_accounting(&result);
            }
            Err(QueryError::BudgetExceeded { .. }) => {}
            Err(error) => panic!("unexpected bounded signature failure: {error:?}"),
        }
    }
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        service.execute_symbol_explain(
            &service
                .plan_symbol_explain(first, QueryBudget::new())
                .unwrap(),
            &cancellation,
        ),
        Err(QueryError::Cancelled(CancellationReason::ClientRequest))
    ));
}

fn occurrence_relation_explain_snapshot() -> (GenerationSnapshot, SymbolId, SymbolId, FactId) {
    let base = fixture_snapshot();
    let mut document = base.document().clone();
    let caller = document.entities[0].id;
    let mut target = document.entities[0].clone();
    target.id = SymbolId::from_bytes([198; 20]);
    target.canonical_name = "callee".to_owned();
    target.display_name = "callee".to_owned();
    target.qualified_name = "crate::callee".to_owned();
    let callee = target.id;
    document.entities.push(target);
    let occurrence_template = document.occurrences[0].clone();
    let relation_template = document.relations[0].clone();
    document.occurrences.clear();
    document.relations.clear();
    document.extensions.clear();
    document.coverage_records.clear();
    let mut sites = Vec::new();
    for index in 0..5_u8 {
        let mut occurrence = occurrence_template.clone();
        occurrence.id = derive_fact("rootlight.query-test.explain-call-site/v1", &[index]).id();
        occurrence.role = OccurrenceRole::CallSite;
        occurrence.enclosing = (index != 3).then_some(caller);
        occurrence.target = OccurrenceTarget::Resolved { symbol: callee };
        occurrence.confidence = Confidence::new(850).unwrap();
        occurrence.source = SourceRef::new(
            document.repository,
            document.generation,
            SourceSpan::new(
                occurrence.file,
                u64::from(index) * 2,
                u64::from(index) * 2 + 1,
            )
            .unwrap(),
            occurrence.source.content_hash(),
            None,
        );
        occurrence.evidence.source = Some(occurrence.source.clone());
        sites.push(occurrence.id);
        document.occurrences.push(occurrence);
    }
    let missing_owner = derive_fact("rootlight.query-test.explain-call-relation/v1", &[4]).id();
    for (index, subject, predicate, object, source_index) in [
        (
            0,
            RelationEndpoint::Occurrence(sites[0]),
            RelationPredicate::Calls,
            RelationEndpoint::Entity(callee),
            0,
        ),
        (
            1,
            RelationEndpoint::Occurrence(sites[1]),
            RelationPredicate::Calls,
            RelationEndpoint::Entity(callee),
            1,
        ),
        (
            2,
            RelationEndpoint::Entity(caller),
            RelationPredicate::Calls,
            RelationEndpoint::Entity(callee),
            0,
        ),
        (
            3,
            RelationEndpoint::Occurrence(sites[2]),
            RelationPredicate::DispatchCandidate,
            RelationEndpoint::Entity(callee),
            2,
        ),
        (
            4,
            RelationEndpoint::Occurrence(sites[3]),
            RelationPredicate::Calls,
            RelationEndpoint::Entity(callee),
            3,
        ),
        (
            5,
            RelationEndpoint::Occurrence(sites[4]),
            RelationPredicate::Calls,
            RelationEndpoint::Entity(caller),
            4,
        ),
        (
            6,
            RelationEndpoint::Entity(callee),
            RelationPredicate::RefersTo,
            RelationEndpoint::Occurrence(sites[0]),
            0,
        ),
    ] {
        let mut relation = relation_template.clone();
        relation.id = derive_fact("rootlight.query-test.explain-call-relation/v1", &[index]).id();
        relation.subject = subject;
        relation.predicate = predicate;
        relation.object = object;
        relation.confidence = Confidence::new(850).unwrap();
        relation.evidence.source = Some(document.occurrences[source_index].source.clone());
        document.relations.push(relation);
    }
    (
        GenerationSnapshot::new(
            base.metadata(),
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap(),
        caller,
        callee,
        missing_owner,
    )
}

#[test]
fn symbol_explain_preserves_occurrence_owned_relations_and_call_sites() {
    let (snapshot, caller, callee, missing_owner) = occurrence_relation_explain_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).unwrap();
    let caller_result = service
        .plan_symbol_explain(caller, QueryBudget::new())
        .and_then(|plan| service.execute_symbol_explain(&plan, &Cancellation::new()))
        .unwrap();
    let expected = snapshot
        .document()
        .relations
        .iter()
        .filter(|relation| relation.id != missing_owner)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(caller_result.data.relations, expected);
    assert_eq!(caller_result.data.relation_endpoints.len(), expected.len());
    for (relation, endpoints) in caller_result
        .data
        .relations
        .iter()
        .zip(&caller_result.data.relation_endpoints)
    {
        assert_eq!(endpoints.relation, relation.id);
        if relation.predicate == RelationPredicate::RefersTo {
            assert_eq!(
                (endpoints.subject, endpoints.object),
                (Some(callee), Some(caller))
            );
        } else {
            assert_eq!(endpoints.subject, Some(caller));
            let object = match relation.object {
                RelationEndpoint::Entity(symbol) => Some(symbol),
                _ => unreachable!(),
            };
            assert_eq!(endpoints.object, object);
        }
    }
    assert!(!caller_result.data.truncated);
    let occurrence_calls = caller_result
        .data
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Calls
                && matches!(relation.subject, RelationEndpoint::Occurrence(_))
                && relation.object == RelationEndpoint::Entity(callee)
        })
        .collect::<Vec<_>>();
    assert_eq!(occurrence_calls.len(), 2);
    assert_ne!(occurrence_calls[0].subject, occurrence_calls[1].subject);
    assert_ne!(
        occurrence_calls[0].evidence.source,
        occurrence_calls[1].evidence.source
    );
    assert!(
        caller_result
            .data
            .relations
            .iter()
            .all(|relation| relation.confidence.get() == 850)
    );
    assert_eq!(
        caller_result
            .data
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::DispatchCandidate)
            .count(),
        1
    );
    assert_exact_response_accounting(&caller_result);
    let callee_result = service
        .plan_symbol_explain(callee, QueryBudget::new())
        .and_then(|plan| service.execute_symbol_explain(&plan, &Cancellation::new()))
        .unwrap();
    assert_eq!(callee_result.data.relations.len(), 6);
    assert!(
        callee_result
            .data
            .relations
            .iter()
            .any(|relation| relation.id == missing_owner)
    );
    let missing = callee_result
        .data
        .relation_endpoints
        .iter()
        .find(|endpoints| endpoints.relation == missing_owner)
        .unwrap();
    assert_eq!((missing.subject, missing.object), (None, Some(callee)));
    assert_exact_response_accounting(&callee_result);
}

#[test]
fn symbol_explain_endpoint_projection_respects_resources_and_cancellation() {
    let (snapshot, caller, _, _) = occurrence_relation_explain_snapshot();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).unwrap();
    let complete = service
        .plan_symbol_explain(caller, QueryBudget::new())
        .and_then(|plan| service.execute_symbol_explain(&plan, &Cancellation::new()))
        .unwrap();
    // The first returned edge is an occurrence-to-entity self edge. Its row
    // limit must include the lookup even though both endpoints project alike.
    assert!(matches!(
        complete.data.relations[0].subject,
        RelationEndpoint::Occurrence(_)
    ));
    assert_eq!(complete.data.relation_endpoints[0].subject, Some(caller));
    assert_eq!(complete.data.relation_endpoints[0].object, Some(caller));
    for (budget, resource) in [
        (QueryBudget::new().with_max_rows(3), QueryResource::Rows),
        (
            QueryBudget::new().with_max_results(3),
            QueryResource::Results,
        ),
        (
            QueryBudget::new().with_max_memory_bytes(complete.usage.memory_bytes - 1),
            QueryResource::MemoryBytes,
        ),
        (QueryBudget::new().with_max_edges(1), QueryResource::Edges),
    ] {
        let result = service
            .plan_symbol_explain(caller, budget)
            .and_then(|plan| service.execute_symbol_explain(&plan, &Cancellation::new()))
            .unwrap();
        assert!(result.data.truncated);
        assert_eq!(result.data.limiting_resources, vec![resource]);
        assert_eq!(
            result.data.relations.len(),
            result.data.relation_endpoints.len()
        );
        for (relation, endpoints) in result
            .data
            .relations
            .iter()
            .zip(&result.data.relation_endpoints)
        {
            assert_eq!(relation.id, endpoints.relation);
        }
        assert!(result.usage.rows <= budget.max_rows());
        assert!(result.usage.results <= budget.max_results());
        assert!(result.usage.memory_bytes <= budget.max_memory_bytes());
        assert_exact_response_accounting(&result);
    }
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    let plan = service
        .plan_symbol_explain(caller, QueryBudget::new())
        .unwrap();
    assert!(matches!(
        service.execute_symbol_explain(&plan, &cancellation),
        Err(QueryError::Cancelled(CancellationReason::ClientRequest))
    ));
}

fn explain_posting_snapshot(unrelated_count: u8) -> (GenerationSnapshot, SymbolId) {
    let (base, symbol) = signature_snapshot(&["fn fixture()"]);
    let mut document = base.document().clone();
    let mut unrelated_entity = document.entities[0].clone();
    unrelated_entity.id = SymbolId::from_bytes([200; 20]);
    unrelated_entity.canonical_name = "unrelated".to_owned();
    unrelated_entity.display_name = "unrelated".to_owned();
    unrelated_entity.qualified_name = "crate::unrelated".to_owned();
    let unrelated_symbol = unrelated_entity.id;
    document.entities.push(unrelated_entity);

    let mut other_file = document.files[0].clone();
    let path = RelativePath::parse(Path::new("src/consumer.rs")).unwrap();
    other_file.id = derive_file(FileIdentity {
        repository: document.repository,
        path_identity: path.identity_bytes(),
    })
    .id();
    other_file.path = path.as_str().to_owned();
    other_file.path_locator = Some(path.to_locator());
    let source = SourceRef::new(
        document.repository,
        document.generation,
        SourceSpan::new(other_file.id, 0, 7).unwrap(),
        other_file.content_hash,
        None,
    );
    other_file.evidence.source = Some(SourceRef::new(
        document.repository,
        document.generation,
        SourceSpan::new(other_file.id, 0, other_file.byte_length).unwrap(),
        other_file.content_hash,
        None,
    ));
    let mut candidate = document.occurrences[0].clone();
    candidate.id = derive_fact("rootlight.query-test.candidate/v1", b"consumer").id();
    candidate.file = other_file.id;
    candidate.source = source.clone();
    candidate.role = OccurrenceRole::Reference;
    candidate.enclosing = Some(unrelated_symbol);
    candidate.target = OccurrenceTarget::Candidates {
        symbols: vec![symbol, unrelated_symbol],
        total_count: 2,
        completeness: CoverageStatus::Complete,
    };
    candidate.evidence.source = Some(source);
    document.files.push(other_file);
    document.occurrences.push(candidate);

    for index in 0..unrelated_count {
        let mut relation = document.relations[0].clone();
        relation.id = derive_fact("rootlight.query-test.explain-noise-relation/v1", &[index]).id();
        relation.subject = RelationEndpoint::Entity(unrelated_symbol);
        relation.object = RelationEndpoint::Entity(unrelated_symbol);
        relation.predicate = RelationPredicate::Calls;
        document.relations.push(relation);
        let mut occurrence = document.occurrences[0].clone();
        occurrence.id =
            derive_fact("rootlight.query-test.explain-noise-occurrence/v1", &[index]).id();
        occurrence.target = OccurrenceTarget::Resolved {
            symbol: unrelated_symbol,
        };
        occurrence.enclosing = Some(unrelated_symbol);
        document.occurrences.push(occurrence);
        let evidence = rootlight_ir::LexicalEvidenceV1::from_complete_text(
            rootlight_ir::LexicalEvidenceKind::Signature,
            rootlight_ir::FactRef::Entity(unrelated_symbol),
            rootlight_ir::LexicalEvidenceFormat::SourceText,
            &format!("fn unrelated_{index}()"),
        )
        .unwrap();
        document.extensions.push(
            rootlight_ir::new_lexical_evidence_envelope(
                document.repository,
                document.generation,
                document.entities[0].provenance,
                document.entities[0].evidence.source.clone().unwrap(),
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
fn symbol_explain_rows_are_independent_of_unrelated_facts() {
    let (baseline, symbol) = explain_posting_snapshot(0);
    let search = fixture_search(&baseline);
    let service = QueryService::new(&baseline, &search).unwrap();
    let expected = service
        .plan_symbol_explain(symbol, QueryBudget::new())
        .and_then(|plan| service.execute_symbol_explain(&plan, &Cancellation::new()))
        .unwrap();
    assert!(!expected.data.truncated);
    assert_eq!(expected.data.occurrences.len(), 2);
    assert!(expected.data.occurrences.iter().any(|occurrence| {
        matches!(occurrence.target, OccurrenceTarget::Candidates { .. })
            && occurrence.file
                != expected
                    .data
                    .entity
                    .evidence
                    .source
                    .as_ref()
                    .unwrap()
                    .span()
                    .file()
    }));
    let (noisy, _) = explain_posting_snapshot(16);
    let search = fixture_search(&noisy);
    let service = QueryService::new(&noisy, &search).unwrap();
    for max_rows in [expected.usage.rows, expected.usage.rows + 1] {
        let actual = service
            .plan_symbol_explain(symbol, QueryBudget::new().with_max_rows(max_rows))
            .and_then(|plan| service.execute_symbol_explain(&plan, &Cancellation::new()))
            .unwrap();
        assert_eq!(actual.data, expected.data);
        assert_eq!(actual.usage.rows, expected.usage.rows);
        assert_eq!(actual.usage.edges, expected.usage.edges);
        assert_eq!(actual.usage.source_bytes, expected.usage.source_bytes);
        assert_exact_response_accounting(&actual);
    }
    let bounded = service
        .plan_symbol_explain(
            symbol,
            QueryBudget::new().with_max_rows(expected.usage.rows - 1),
        )
        .and_then(|plan| service.execute_symbol_explain(&plan, &Cancellation::new()))
        .unwrap();
    assert!(bounded.data.truncated);
    assert_eq!(bounded.data.limiting_resources, vec![QueryResource::Rows]);
    assert_eq!(bounded.usage.rows, expected.usage.rows - 1);
    assert_exact_response_accounting(&bounded);
}

#[test]
fn symbol_explain_keeps_own_occurrences_and_signature_before_large_relation_fanout() {
    let (base, symbol) = signature_snapshot(&["fn fixture()"]);
    let mut document = base.document().clone();
    for index in 0..64_u8 {
        let mut relation = document.relations[0].clone();
        relation.id = derive_fact("rootlight.query-test.explain-fanout/v1", &[index]).id();
        relation.subject = RelationEndpoint::Entity(symbol);
        relation.object = RelationEndpoint::Entity(symbol);
        relation.predicate = RelationPredicate::Calls;
        document.relations.push(relation);
    }
    let snapshot = GenerationSnapshot::new(
        base.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).unwrap();
    for (budget, resource) in [
        (QueryBudget::new().with_max_rows(12), QueryResource::Rows),
        (
            QueryBudget::new().with_max_results(5),
            QueryResource::Results,
        ),
    ] {
        let result = service
            .plan_symbol_explain(symbol, budget)
            .and_then(|plan| service.execute_symbol_explain(&plan, &Cancellation::new()))
            .unwrap();
        assert!(result.data.truncated);
        assert!(result.data.limiting_resources.contains(&resource));
        assert!(result.data.occurrences.iter().any(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol }
        }));
        assert_eq!(
            result.data.signature.as_ref().unwrap().text(),
            "fn fixture()"
        );
        assert!(result.usage.rows <= budget.max_rows());
        assert!(result.usage.results <= budget.max_results());
        assert_exact_response_accounting(&result);
    }
}

fn projected_signature_snapshot(
    source_bytes: &[u8],
    encoding: Option<rootlight_ir::SourceTextEncoding>,
    ranges: &[std::ops::Range<usize>],
) -> (
    GenerationSnapshot,
    SymbolId,
    rootlight_ir::LexicalEvidenceV1,
) {
    let (base, source) = fallback_fixture_with_bytes(
        "signature.sourceblob",
        source_bytes.to_vec(),
        encoding.map_or("utf-8", rootlight_ir::SourceTextEncoding::as_str),
    );
    let mut document = base.document().clone();
    let reference = document
        .files
        .iter()
        .find(|file| file.id == source.file())
        .unwrap()
        .evidence
        .source
        .clone()
        .unwrap();
    let entity = &mut document.entities[0];
    let symbol = entity.id;
    let provenance = entity.provenance;
    entity.container = Some(rootlight_ir::ContainerRef::File(source.file()));
    entity.evidence.source = Some(reference.clone());
    let subject = rootlight_ir::FactRef::Entity(symbol);
    let evidence = match encoding {
        Some(encoding) => rootlight_ir::LexicalEvidenceV1::from_encoded_source_projection(
            subject,
            rootlight_ir::EncodedSourceTextProjection::from_source(source_bytes, encoding, ranges)
                .unwrap(),
        ),
        None => rootlight_ir::LexicalEvidenceV1::from_source_projection(
            subject,
            rootlight_ir::SourceTextProjection::from_source(
                std::str::from_utf8(source_bytes).unwrap(),
                ranges,
            )
            .unwrap(),
        ),
    };
    assert_eq!(evidence.verify_source_bytes(source_bytes), Ok(()));
    document.extensions.push(
        rootlight_ir::new_lexical_evidence_envelope(
            document.repository,
            document.generation,
            provenance,
            reference,
            &evidence,
        )
        .unwrap(),
    );
    (
        GenerationSnapshot::new(
            base.metadata(),
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap(),
        symbol,
        evidence,
    )
}

fn assert_signature_physical_budget(
    snapshot: &GenerationSnapshot,
    symbol: SymbolId,
    evidence: &rootlight_ir::LexicalEvidenceV1,
    physical_bytes: u64,
) {
    let search = fixture_search(snapshot);
    let service = QueryService::new(snapshot, &search).unwrap();
    for maximum in [physical_bytes - 1, physical_bytes, physical_bytes + 1] {
        let plan = service
            .plan_symbol_explain(symbol, QueryBudget::new().with_max_source_bytes(maximum))
            .unwrap();
        let result = service
            .execute_symbol_explain(&plan, &Cancellation::new())
            .unwrap();
        if maximum < physical_bytes {
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
            assert_eq!(result.data.signature.as_ref(), Some(evidence));
            assert_eq!(result.usage.source_bytes, physical_bytes);
            assert!(!result.data.truncated);
            assert_eq!(
                result.data.relations.len(),
                result.data.relation_endpoints.len()
            );
            assert_eq!(
                result.usage.results,
                u64::try_from(
                    3 + result.data.relations.len()
                        + result.data.relation_endpoints.len()
                        + result.data.occurrences.len()
                        + result.data.coverage.len(),
                )
                .unwrap()
            );
        }
        assert!(result.usage.source_bytes <= plan.explanation().estimate.source_bytes);
        assert_exact_response_accounting(&result);
    }
}

#[test]
fn projected_signatures_charge_only_physical_fragments() {
    for (source, ranges) in [
        ("fn /* gap */ item", [0..2, 13..17]),
        ("fnitem", [0..2, 2..6]),
    ] {
        let (snapshot, symbol, evidence) =
            projected_signature_snapshot(source.as_bytes(), None, &ranges);
        assert_eq!(evidence.text(), "fn item");
        assert_eq!(evidence.text().len(), 7);
        assert_eq!(evidence.source_parts().unwrap().len(), 2);
        assert_signature_physical_budget(&snapshot, symbol, &evidence, 6);
    }
}

#[test]
fn encoded_signatures_charge_physical_source_in_both_endians() {
    for encoding in [
        rootlight_ir::SourceTextEncoding::Utf16Le,
        rootlight_ir::SourceTextEncoding::Utf16Be,
    ] {
        for (source, ranges, retained, physical_bytes, text_bytes) in [
            (
                "\u{feff}\u{2603}",
                std::iter::once(2..4).collect::<Vec<_>>(),
                "\u{2603}",
                2,
                3,
            ),
            (
                "\u{feff}x",
                std::iter::once(2..4).collect::<Vec<_>>(),
                "x",
                2,
                1,
            ),
            (
                "\u{feff}\u{1f642}",
                std::iter::once(2..6).collect::<Vec<_>>(),
                "\u{1f642}",
                4,
                4,
            ),
            (
                "\u{feff}\u{2603} /* skipped */ x",
                vec![2..4, 34..36],
                "\u{2603} x",
                4,
                5,
            ),
        ] {
            let raw = source
                .encode_utf16()
                .flat_map(|unit| match encoding {
                    rootlight_ir::SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
                    rootlight_ir::SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
                })
                .collect::<Vec<_>>();
            let (snapshot, symbol, evidence) =
                projected_signature_snapshot(&raw, Some(encoding), &ranges);
            assert_eq!(evidence.text(), retained);
            assert_eq!(evidence.text().len(), text_bytes);
            assert_eq!(evidence.source_encoding(), Some(encoding));
            assert_signature_physical_budget(&snapshot, symbol, &evidence, physical_bytes);
        }
    }
}

#[test]
fn encoded_signature_plan_bounds_physical_bytes_above_text_limit() {
    let retained = "x".repeat(3_000);
    let source = format!("\u{feff}{retained}");
    for encoding in [
        rootlight_ir::SourceTextEncoding::Utf16Le,
        rootlight_ir::SourceTextEncoding::Utf16Be,
    ] {
        let raw = source
            .encode_utf16()
            .flat_map(|unit| match encoding {
                rootlight_ir::SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
                rootlight_ir::SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
            })
            .collect::<Vec<_>>();
        let range = 2..raw.len();
        let (snapshot, symbol, evidence) =
            projected_signature_snapshot(&raw, Some(encoding), std::slice::from_ref(&range));
        assert_eq!(evidence.text(), retained);
        let physical_bytes = u64::try_from(range.len()).unwrap();
        assert!(physical_bytes > u64::try_from(rootlight_ir::MAX_LEXICAL_SIGNATURE_BYTES).unwrap());
        let search = fixture_search(&snapshot);
        let service = QueryService::new(&snapshot, &search).unwrap();
        let plan = service
            .plan_symbol_explain(
                symbol,
                QueryBudget::new().with_max_source_bytes(physical_bytes),
            )
            .unwrap();
        assert_eq!(plan.explanation().estimate.source_bytes, physical_bytes);
        assert_signature_physical_budget(&snapshot, symbol, &evidence, physical_bytes);
    }
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
fn retained_signatures_reach_symbol_projection_and_text_locate() {
    let text = "fn item(value: HeaderWitness)";
    let (snapshot, symbol) = signature_snapshot(&[text]);
    let cancellation = Cancellation::new();
    let projected =
        project_lexical_documents(&snapshot, BuildBudget::default(), &cancellation).unwrap();
    let document = projected
        .iter()
        .find(|document| document.symbol_id == Some(symbol))
        .unwrap();
    assert_eq!(document.signature.as_deref(), Some(text));
    let search = rootlight_search::LexicalIndex::build_ephemeral(
        snapshot.metadata().generation(),
        projected,
        BuildBudget::default(),
        &cancellation,
    )
    .unwrap();
    let service = QueryService::new(&snapshot, &search).unwrap();
    let plan = service
        .plan_code_locate(
            "HeaderWitness".to_owned(),
            LocateMode::Text,
            10,
            0,
            SearchBudget::default(),
            QueryBudget::new(),
        )
        .unwrap();
    let result = service.execute_code_locate(&plan, &cancellation).unwrap();
    assert_eq!(result.data.hits.len(), 1);
    let hit = &result.data.hits[0];
    assert_eq!(hit.symbol, Some(symbol));
    assert_eq!(
        hit.source.as_ref(),
        snapshot.document().entities[0].evidence.source.as_ref()
    );
    let signature = hit.signature.as_ref().unwrap();
    assert_eq!(signature.text(), text);
    assert_eq!(signature.subject(), rootlight_ir::FactRef::Entity(symbol));
    assert_eq!(
        signature.complete_text_hash(),
        content_hash(text.as_bytes())
    );
    assert!(!signature.is_truncated());
    let explained = service
        .execute_symbol_explain(
            &service
                .plan_symbol_explain(symbol, QueryBudget::new())
                .unwrap(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(hit.signature, explained.data.signature);
    assert!(!result.data.truncated);
    assert_exact_response_accounting(&result);
}

#[test]
fn signature_projection_and_locate_reject_ambiguous_retained_evidence() {
    let text = "fn item(value: HeaderWitness)";
    for (signatures, expected) in [
        (Vec::new(), None),
        (vec![text, "fn item(value: OtherHeaderWitness)"], None),
        (vec![text, text], Some(text)),
    ] {
        let (snapshot, symbol) = signature_snapshot(&signatures);
        let cancellation = Cancellation::new();
        let projected =
            project_lexical_documents(&snapshot, BuildBudget::default(), &cancellation).unwrap();
        assert_eq!(
            projected
                .iter()
                .find(|document| document.symbol_id == Some(symbol))
                .unwrap()
                .signature
                .as_deref(),
            expected
        );
        let index = rootlight_search::LexicalIndex::build_ephemeral(
            snapshot.metadata().generation(),
            projected,
            BuildBudget::default(),
            &cancellation,
        )
        .unwrap();
        let service = QueryService::new(&snapshot, &index).unwrap();
        let plan = service
            .plan_code_locate(
                "HeaderWitness".to_owned(),
                LocateMode::Text,
                10,
                0,
                SearchBudget::default(),
                QueryBudget::new(),
            )
            .unwrap();
        let lexical = service.execute_code_locate(&plan, &cancellation).unwrap();
        assert_eq!(lexical.data.hits.len(), usize::from(expected.is_some()));
        if let Some(hit) = lexical.data.hits.first() {
            assert_eq!(hit.symbol, Some(symbol));
            assert_eq!(
                hit.signature.as_ref().map(|evidence| evidence.text()),
                expected
            );
        }
        let exact = fixture_search(&snapshot);
        let service = QueryService::new(&snapshot, &exact).unwrap();
        let plan = service
            .plan_code_locate(
                exact.hits[0].identifier.clone(),
                LocateMode::Exact,
                1,
                0,
                SearchBudget::default(),
                QueryBudget::new(),
            )
            .unwrap();
        let located = service.execute_code_locate(&plan, &cancellation).unwrap();
        assert_eq!(located.data.hits.len(), 1);
        assert_eq!(
            located.data.hits[0]
                .signature
                .as_ref()
                .map(|evidence| evidence.text()),
            expected
        );
        assert!(!located.data.truncated);
        assert_exact_response_accounting(&located);
    }
}

#[test]
fn retained_signature_projection_charges_exact_build_text_bytes() {
    let text = "fn item(value: HeaderWitness)";
    let cancellation = Cancellation::new();
    let (without_signature, _) = signature_snapshot(&[]);
    let documents =
        project_lexical_documents(&without_signature, BuildBudget::default(), &cancellation)
            .unwrap();
    let metadata_bytes = documents
        .iter()
        .map(|document| {
            document.identifier.len()
                + document.canonical_name.as_ref().map_or(0, String::len)
                + document.qualified_name.len()
                + document.path.len()
                + document.kind.len()
                + document.language.len()
                + document.tier.len()
        })
        .sum::<usize>();
    let (snapshot, symbol) = signature_snapshot(&[text]);
    for allowance in [metadata_bytes + text.len() - 1, metadata_bytes + text.len()] {
        let projected = project_lexical_documents(
            &snapshot,
            BuildBudget {
                max_text_bytes: allowance,
                ..BuildBudget::default()
            },
            &cancellation,
        );
        if allowance < metadata_bytes + text.len() {
            assert!(matches!(
                projected,
                Err(QueryError::Search(SearchError::BuildBudgetExceeded {
                    resource: "text_bytes"
                }))
            ));
        } else {
            assert_eq!(
                projected
                    .unwrap()
                    .iter()
                    .find(|document| document.symbol_id == Some(symbol))
                    .unwrap()
                    .signature
                    .as_deref(),
                Some(text)
            );
        }
    }
}

#[test]
fn locate_signatures_obey_source_byte_budgets() {
    let text = "fn item(value: é) -> Output";
    let bytes = u64::try_from(text.len()).unwrap();
    let (snapshot, symbol) = signature_snapshot(&[text]);
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).unwrap();
    for maximum in [bytes - 1, bytes, bytes + 1] {
        let plan = service
            .plan_code_locate(
                search.hits[0].identifier.clone(),
                LocateMode::Exact,
                1,
                0,
                SearchBudget::default(),
                QueryBudget::new().with_max_source_bytes(maximum),
            )
            .unwrap();
        let result = service
            .execute_code_locate(&plan, &Cancellation::new())
            .unwrap();
        assert_eq!(result.data.hits.len(), 1);
        assert_eq!(result.data.hits[0].symbol, Some(symbol));
        if maximum < bytes {
            assert!(result.data.hits[0].signature.is_none());
            assert_eq!(result.usage.source_bytes, 0);
            assert!(result.data.truncated);
            assert!(
                result
                    .data
                    .limiting_resources
                    .contains(&QueryResource::SourceBytes)
            );
        } else {
            assert_eq!(result.data.hits[0].signature.as_ref().unwrap().text(), text);
            assert_eq!(result.usage.source_bytes, bytes);
            assert!(!result.data.truncated);
        }
        assert!(result.usage.source_bytes <= plan.explanation().estimate.source_bytes);
        assert_exact_response_accounting(&result);
    }
}

#[test]
fn incomplete_locate_signature_scans_never_publish_evidence() {
    let text = format!("fn item({})", "value: HeaderWitness, ".repeat(50));
    let (snapshot, _) = signature_snapshot(&[&text]);
    let search = fixture_search(&snapshot);
    let service = QueryService::new(&snapshot, &search).unwrap();
    for (budget, resource) in [
        (QueryBudget::new().with_max_rows(2), QueryResource::Rows),
        (
            QueryBudget::new().with_max_memory_bytes(1_024),
            QueryResource::MemoryBytes,
        ),
    ] {
        let plan = service
            .plan_code_locate(
                search.hits[0].identifier.clone(),
                LocateMode::Exact,
                1,
                0,
                SearchBudget {
                    max_candidates: 1,
                    ..SearchBudget::default()
                },
                budget,
            )
            .unwrap();
        match service.execute_code_locate(&plan, &Cancellation::new()) {
            Ok(result) => {
                assert!(result.data.hits.iter().all(|hit| hit.signature.is_none()));
                assert!(result.data.truncated);
                assert!(result.data.limiting_resources.contains(&resource));
                assert!(result.usage.rows <= budget.max_rows());
                assert!(result.usage.memory_bytes <= budget.max_memory_bytes());
                assert_exact_response_accounting(&result);
            }
            Err(QueryError::BudgetExceeded {
                resource: exceeded, ..
            }) => assert_eq!(exceeded, resource),
            Err(error) => panic!("unexpected locate signature failure: {error:?}"),
        }
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
    assert!(
        !snapshot.document().source_mappings.is_empty(),
        "fixture must exercise bounded origin verification"
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

    assert!(
        response.data.hits.is_empty(),
        "unverified origins must not escape a truncated scan"
    );
    assert!(response.usage.rows <= 3);
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
