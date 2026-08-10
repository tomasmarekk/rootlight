//! Public-boundary proof for the daemon-independent first slice.

use std::{
    collections::BTreeSet,
    fs,
    time::{Duration, Instant},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, RepositoryId, content_hash};
use rootlight_incremental::FactDomain;
use rootlight_ir::CoverageStatus;
use rootlight_query::{
    ADVANCED_DEFAULT_MAX_DEPTH, ADVANCED_MAX_TRAVERSAL, AdvancedAggregateFunction, AdvancedAstNode,
    AdvancedCompleteness, AdvancedEntityKind, HistoryCompareScope, LocateMode, RepositoryDataTrust,
};
use rootlight_service::{
    ArchitectureOverviewView, ChangeClass, ChangeImpactClassification, ChangeImpactRiskLevel,
    CodeDeadEntryPointPolicy, FileChangeKind, FirstSliceBudget, FirstSliceBuildStrategy,
    FirstSliceError, FirstSliceFreshnessStatus, FirstSliceIncrementalEvidence,
    FirstSliceObservedFreshness, FirstSlicePublicationMode, FirstSliceService,
    FirstSliceTwoStageAvailability, PlanChangeObjective, RUNTIME_TRACE_SCHEMA_VERSION,
    RelationDirection, RelationFamily, RuntimeTraceLimits, SharedGenerationExpectation,
    SharedGenerationLimits,
    catalog::{
        CatalogInstant, CatalogListFilter, CatalogPageRequest, CatalogPageSize,
        CatalogRepositoryState,
    },
};
use tempfile::TempDir;

const BEFORE: &str = "pub fn answer() -> u32 {\n    42\n}\n";
const AFTER: &str = "pub fn answer() -> u32 {\n    43\n}\n";
const OTHER: &str = "pub fn other() -> u32 {\n    7\n}\n";
const KEPT: &str = "pub fn kept_after_negation() -> bool {\n    true\n}\n";
const MALFORMED: &str = "// malformed_source_sentinel\npub fn broken( {\n";

#[test]
fn shared_generation_round_trip_is_source_bound_and_does_not_activate() {
    let fixture = fixture(BEFORE);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

    let exported = service
        .export_shared_generation(
            indexed.repository,
            Some(indexed.generation),
            SharedGenerationLimits::default(),
            &cancellation,
        )
        .expect("generation exports");
    assert_eq!(exported.repository(), indexed.repository);
    assert_eq!(exported.generation(), indexed.generation);

    let imported = service
        .import_shared_generation(
            exported.bundle(),
            SharedGenerationExpectation::new(indexed.repository, exported.source_set_hash())
                .with_generation(indexed.generation),
            SharedGenerationLimits::default(),
            &cancellation,
        )
        .expect("generation imports");
    assert_eq!(
        imported.generation().metadata().generation(),
        indexed.generation
    );
    assert_eq!(
        service.active_generation_for(indexed.repository),
        Some(indexed.generation)
    );

    assert_eq!(
        service
            .import_shared_generation(
                exported.bundle(),
                SharedGenerationExpectation::new(
                    indexed.repository,
                    content_hash(b"wrong source set"),
                ),
                SharedGenerationLimits::default(),
                &cancellation,
            )
            .expect_err("wrong source set is rejected"),
        FirstSliceError::Sharing
    );
}

#[test]
fn runtime_trace_import_is_generation_bound_and_never_mutates_static_state() {
    let fixture = fixture(BEFORE);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");
    let located = service
        .code_locate(
            indexed.generation,
            "answer".to_owned(),
            LocateMode::Exact,
            1,
            0,
            &cancellation,
        )
        .expect("fixture symbol locates");
    let symbol = located.data.hits[0].symbol;
    let trace = serde_json::to_vec(&serde_json::json!({
        "schema": RUNTIME_TRACE_SCHEMA_VERSION,
        "repository": indexed.repository,
        "generation": indexed.generation,
        "producer": {
            "name": "vertical-slice-tracer",
            "version": "1.0.0",
            "configuration_hash": content_hash(b"vertical-slice-tracer-config"),
            "binary_digest": content_hash(b"vertical-slice-tracer-binary"),
        },
        "records": [{
            "kind": "calls",
            "subject": symbol,
            "object": symbol,
            "count": 3,
        }],
    }))
    .expect("runtime trace encodes");

    let active_before = service.active_generation_for(indexed.repository);
    let overlay = service
        .import_runtime_trace_overlay(
            indexed.repository,
            indexed.generation,
            &trace,
            RuntimeTraceLimits::default(),
            &cancellation,
        )
        .expect("runtime trace imports");

    assert_eq!(overlay.repository(), indexed.repository);
    assert_eq!(overlay.generation(), indexed.generation);
    assert_eq!(overlay.relations().len(), 1);
    assert_eq!(overlay.total_observations(), 3);
    assert_eq!(
        service.active_generation_for(indexed.repository),
        active_before
    );
    assert_eq!(
        service
            .import_runtime_trace_overlay(
                indexed.repository,
                GenerationId::from_bytes([0xff; 20]),
                &trace,
                RuntimeTraceLimits::default(),
                &cancellation,
            )
            .expect_err("an unretained generation fails before trace import"),
        FirstSliceError::GenerationNotFound
    );
    assert_eq!(
        service.active_generation_for(indexed.repository),
        active_before
    );
}

#[test]
fn fixture_flows_through_oracle_search_queries_and_prior_generation() {
    let fixture = TempDir::new().expect("fixture root exists");
    fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
    let source_path = fixture.path().join("src/lib.rs");
    fs::write(&source_path, BEFORE).expect("first fixture source writes");
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");

    let first = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("first fixture generation indexes");
    assert_eq!(first.discovered_inputs, 1);
    assert_eq!(first.indexed_files, 1);
    assert!(first.entities > 0);
    assert!(first.lexical_documents > 0);
    assert!(first.oracle_allocated_bytes > 0);
    let initial_evidence = service
        .incremental_evidence(first.generation)
        .expect("initial incremental evidence is retained");
    assert_eq!(
        initial_evidence.strategy(),
        FirstSliceBuildStrategy::Initial
    );
    assert_eq!(initial_evidence.fallback_reason(), None);
    assert_eq!(initial_evidence.parsed_files(), 1);
    assert_eq!(initial_evidence.reused_parser_artifacts(), 0);
    assert_eq!(initial_evidence.lowered_files(), 1);
    assert!(initial_evidence.structural_cache_retained());
    assert_eq!(
        service
            .generation_freshness(first.repository, first.generation)
            .expect("initial freshness is available"),
        current_process_local_freshness()
    );
    let repeated = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("unchanged request is idempotent");
    assert_eq!(repeated, first);
    let alias = fixture.path().join(".");
    let repeated_through_alias = service
        .index_rust_fixture(&alias, &cancellation)
        .expect("canonical root alias is idempotent");
    assert_eq!(repeated_through_alias, first);

    #[cfg(windows)]
    {
        let case_alias = fixture.path().to_string_lossy().to_ascii_uppercase();
        let case_alias = std::path::Path::new(&case_alias);
        if case_alias.is_dir() {
            let repeated_through_case_alias = service
                .index_rust_fixture(case_alias, &cancellation)
                .expect("case-insensitive root alias is idempotent");
            assert_eq!(repeated_through_case_alias, first);
        }
    }

    let located = service
        .code_locate(
            first.generation,
            "answer".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate query succeeds");
    assert_eq!(located.data.hits.len(), 1);
    assert_eq!(
        located.data.hits[0].trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
    assert!(!located.data.coverage.is_empty());
    let symbol = located.data.hits[0].symbol;
    let reference = located.data.hits[0]
        .source
        .clone()
        .expect("located symbol has exact source evidence");
    let explained = service
        .symbol_explain(first.generation, symbol, &cancellation)
        .expect("explain query succeeds");
    assert_eq!(explained.data.entity.id, symbol);
    assert!(!explained.data.coverage.is_empty());
    let source = service
        .source_read(first.generation, vec![reference.clone()], &cancellation)
        .expect("source query succeeds");
    assert_eq!(source.data.chunks.len(), 1);
    assert!(String::from_utf8_lossy(&source.data.chunks[0].bytes).contains("answer"));
    assert_eq!(
        source.data.chunks[0].trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );

    fs::write(&source_path, AFTER).expect("second fixture source writes");
    let second = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("second fixture generation indexes");
    assert_eq!(second.parent, Some(first.generation));
    assert_ne!(second.generation, first.generation);
    assert_eq!(service.active_generation(), Some(second.generation));
    let incremental_evidence = service
        .incremental_evidence(second.generation)
        .expect("successor incremental evidence is retained");
    assert_dependency_directed_rebuild(incremental_evidence);
    assert_eq!(
        input_change_count(incremental_evidence, ChangeClass::BodyOnly),
        1
    );
    assert_eq!(
        input_change_count(incremental_evidence, ChangeClass::Surface),
        0
    );
    assert_eq!(
        file_change_count(incremental_evidence, FileChangeKind::Modified),
        1
    );
    assert_eq!(incremental_evidence.hashed_files(), 1);
    assert_eq!(
        service
            .generation_freshness(first.repository, first.generation)
            .expect("superseded freshness is available")
            .structural,
        FirstSliceObservedFreshness::Superseded
    );
    assert_eq!(
        service
            .generation_freshness(second.repository, second.generation)
            .expect("successor freshness is available"),
        current_process_local_freshness()
    );
    let pinned_first = service
        .code_locate(
            first.generation,
            "answer".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("old generation remains queryable");
    assert_eq!(pinned_first.data, located.data);

    let active = service
        .code_locate(
            second.generation,
            "answer".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("active generation remains queryable");
    let active_reference = active.data.hits[0]
        .source
        .clone()
        .expect("active symbol has exact source evidence");
    let active_source = service
        .source_read(
            second.generation,
            vec![active_reference.clone()],
            &cancellation,
        )
        .expect("active source snapshot remains readable");
    assert_eq!(active_source.data.chunks[0].bytes, AFTER.as_bytes());
    assert_eq!(
        active_source.data.chunks[0].content_hash,
        active_reference.content_hash()
    );

    let pinned_source = service
        .source_read(first.generation, vec![reference.clone()], &cancellation)
        .expect("superseded source snapshot remains readable");
    assert_eq!(pinned_source.data.chunks[0].bytes, BEFORE.as_bytes());
    assert_eq!(
        pinned_source.data.chunks[0].content_hash,
        reference.content_hash()
    );

    assert!(matches!(
        service.source_read(
            GenerationId::from_bytes([0x55; 20]),
            vec![reference],
            &cancellation,
        ),
        Err(FirstSliceError::Query)
    ));
}

#[test]
fn repository_lineage_survives_interleaved_indexing() {
    let first_fixture = fixture(BEFORE);
    let second_fixture = fixture(OTHER);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(4).expect("first-slice service initializes");

    let first = service
        .index_rust_fixture(first_fixture.path(), &cancellation)
        .expect("first repository indexes");
    let second = service
        .index_rust_fixture(second_fixture.path(), &cancellation)
        .expect("second repository indexes");
    assert_ne!(first.repository, second.repository);
    assert_eq!(
        service.active_generation_for(first.repository),
        Some(first.generation)
    );
    assert_eq!(
        service.active_generation_for(second.repository),
        Some(second.generation)
    );
    assert_eq!(
        service
            .resolve_generation(first.repository, Some(first.generation))
            .expect("owned generation resolves")
            .receipt,
        first
    );
    assert_eq!(
        service.resolve_generation(first.repository, Some(second.generation)),
        Err(FirstSliceError::GenerationMismatch)
    );
    assert_eq!(
        service.resolve_generation(first.repository, Some(GenerationId::from_bytes([0x55; 20]))),
        Err(FirstSliceError::GenerationNotFound)
    );
    assert_eq!(
        service.resolve_generation(RepositoryId::from_bytes([0x44; 16]), None),
        Err(FirstSliceError::RepositoryNotFound)
    );

    let repeated_first = service
        .index_rust_fixture(first_fixture.path(), &cancellation)
        .expect("unchanged first repository reactivates");
    assert_eq!(repeated_first, first);
    assert_eq!(service.active_generation(), Some(first.generation));

    fs::write(first_fixture.path().join("src/lib.rs"), AFTER)
        .expect("changed first fixture writes");
    let changed_first = service
        .index_rust_fixture(first_fixture.path(), &cancellation)
        .expect("changed first repository indexes");
    assert_eq!(changed_first.parent, Some(first.generation));
    assert_ne!(changed_first.generation, first.generation);
    let superseded = service
        .resolve_generation(first.repository, Some(first.generation))
        .expect("prior generation remains owned and retained");
    assert!(!superseded.active);
    assert_eq!(
        service
            .resolve_generation(first.repository, None)
            .expect("active generation resolves")
            .generation,
        changed_first.generation
    );
}

#[test]
fn cancellation_stays_typed_across_index_and_query_boundaries() {
    let fixture = fixture(BEFORE);
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let cancelled_index = deadline();
    assert!(cancelled_index.cancel(CancellationReason::ClientRequest));
    assert_eq!(
        service.index_rust_fixture(fixture.path(), &cancelled_index),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    );
    assert_eq!(service.active_generation(), None);

    let indexed = service
        .index_rust_fixture(fixture.path(), &deadline())
        .expect("fixture indexes before query cancellation");
    let cancelled_query = deadline();
    assert!(cancelled_query.cancel(CancellationReason::ParentCancelled));
    assert!(matches!(
        service.code_locate(
            indexed.generation,
            "answer".to_owned(),
            LocateMode::Exact,
            1,
            0,
            &cancelled_query,
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ParentCancelled
        ))
    ));

    let symbol = service
        .code_locate(
            indexed.generation,
            "answer".to_owned(),
            LocateMode::Exact,
            1,
            0,
            &deadline(),
        )
        .expect("fixture symbol remains queryable")
        .data
        .hits[0]
        .symbol;
    let cancelled = deadline();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        service.symbol_relationships(
            indexed.generation,
            BTreeSet::from([symbol]),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            8,
            0,
            &cancelled,
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert!(matches!(
        service.flow_trace(
            indexed.generation,
            symbol,
            None,
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            3,
            8,
            &cancelled,
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert!(matches!(
        service.architecture_cycles(
            indexed.generation,
            vec![RelationFamily::Calls],
            2,
            8,
            false,
            &cancelled,
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert!(matches!(
        service.code_dead(
            indexed.generation,
            CodeDeadEntryPointPolicy::Standard,
            false,
            false,
            0,
            8,
            &cancelled,
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert!(matches!(
        service.architecture_overview(
            indexed.generation,
            vec![ArchitectureOverviewView::Hotspots],
            0,
            8,
            true,
            &cancelled,
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
}

#[test]
fn invalid_utf8_publishes_a_bounded_successor_without_losing_the_prior_generation() {
    let fixture = fixture(BEFORE);
    let source_path = fixture.path().join("src/lib.rs");
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let first = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("valid generation indexes");
    let mut invalid_utf8 = BEFORE.as_bytes().to_vec();
    invalid_utf8.push(0xff);
    fs::write(source_path, invalid_utf8).expect("invalid UTF-8 source writes");

    let second = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("invalid UTF-8 publishes bounded file coverage");
    assert_eq!(second.parent, Some(first.generation));
    assert!(second.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "invalid-utf8" && diagnostic.message == "source is not valid utf-8"
    }));
    assert_eq!(
        service.active_generation_for(first.repository),
        Some(second.generation)
    );
    assert_eq!(
        service
            .resolve_generation(first.repository, Some(first.generation))
            .expect("prior generation remains retained")
            .receipt,
        first
    );
}

#[test]
fn prepared_generation_is_not_queryable_before_publication() {
    let fixture = fixture(BEFORE);
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let cancellation = deadline();
    let prepared = service
        .prepare_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture prepares");
    let receipt = prepared.receipt();
    let staged = service
        .stage_prepared(prepared, &cancellation)
        .expect("fixture enters hidden staging");
    assert_eq!(service.active_generation_for(receipt.repository), None);
    assert_eq!(
        service.resolve_generation(receipt.repository, Some(receipt.generation)),
        Err(FirstSliceError::RepositoryNotFound)
    );
    assert!(matches!(
        service.code_locate(
            receipt.generation,
            "answer".to_owned(),
            LocateMode::Exact,
            1,
            0,
            &cancellation,
        ),
        Err(FirstSliceError::Query)
    ));

    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    service
        .discard_staged(staged)
        .expect("cancelled staging reservation releases");
    assert_eq!(service.active_generation_for(receipt.repository), None);

    let publication = deadline();
    let prepared = service
        .prepare_rust_fixture(fixture.path(), &publication)
        .expect("fixture prepares again");
    let staged = service
        .stage_prepared(prepared, &publication)
        .expect("fixture stages again");
    let published = service
        .commit_staged(staged)
        .expect("authorized publication succeeds");
    assert_eq!(published.discovered_inputs, receipt.discovered_inputs);
    assert_eq!(published.indexed_files, receipt.indexed_files);
    assert_eq!(published.entities, receipt.entities);
    assert_eq!(published.lexical_documents, receipt.lexical_documents);
    assert_eq!(
        service.active_generation_for(published.repository),
        Some(published.generation)
    );
}

#[test]
fn rust_repository_indexes_sources_and_explicit_dispositions_with_lineage() {
    let fixture = TempDir::new().expect("fixture root exists");
    fs::create_dir_all(fixture.path().join("src/nested")).expect("fixture source directory exists");
    fs::write(
        fixture.path().join("Cargo.toml"),
        "[package]\nname = \"cargo_manifest_sentinel\"\nversion = \"0.0.0\"\n",
    )
    .expect("fixture manifest writes");
    fs::write(
        fixture.path().join("src/nested/.gitignore"),
        "nested_ignore_sentinel\n",
    )
    .expect("fixture ignore file writes");
    fs::write(fixture.path().join("src/lib.rs"), BEFORE).expect("primary source writes");
    fs::write(fixture.path().join("src/nested/kept.rs"), KEPT).expect("kept source writes");
    fs::write(fixture.path().join("src/malformed.rs"), MALFORMED).expect("malformed source writes");

    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let first = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("multi-file Rust repository indexes");
    assert_eq!(first.discovered_inputs, 5);
    assert_eq!(first.indexed_files, 5);

    let answer = service
        .code_locate(
            first.generation,
            "answer".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("answer locate succeeds");
    assert_eq!(answer.data.hits.len(), 1);
    assert_eq!(answer.data.hits[0].path, "src/lib.rs");
    let first_answer = answer.data.hits[0]
        .source
        .clone()
        .expect("answer retains exact source evidence");
    let pinned_source = service
        .source_read(first.generation, vec![first_answer], &cancellation)
        .expect("first generation source is queryable");
    let pinned_text = String::from_utf8_lossy(&pinned_source.data.chunks[0].bytes);
    assert!(pinned_text.contains("42"));
    assert!(!pinned_text.contains("43"));

    let kept = service
        .code_locate(
            first.generation,
            "kept_after_negation".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("nested kept source locate succeeds");
    assert_eq!(kept.data.hits.len(), 1);
    assert_eq!(kept.data.hits[0].path, "src/nested/kept.rs");

    for sentinel in [
        "cargo_manifest_sentinel",
        "nested_ignore_sentinel",
        "malformed_source_sentinel",
    ] {
        let located = service
            .code_locate(
                first.generation,
                sentinel.to_owned(),
                LocateMode::Exact,
                8,
                0,
                &cancellation,
            )
            .expect("non-source sentinel locate succeeds");
        assert!(
            located.data.hits.is_empty(),
            "{sentinel} must not be indexed"
        );
    }

    let repeated = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("unchanged multi-file repository is idempotent");
    assert_eq!(repeated, first);

    fs::write(fixture.path().join("src/lib.rs"), AFTER).expect("changed source writes");
    let second = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("changed multi-file repository indexes");
    assert_eq!(second.parent, Some(first.generation));
    assert_ne!(second.generation, first.generation);

    let second_answer = service
        .code_locate(
            second.generation,
            "answer".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("active answer locate succeeds");
    assert_eq!(second_answer.data.hits.len(), 1);
    assert_eq!(second_answer.data.hits[0].path, "src/lib.rs");
    let second_answer = second_answer.data.hits[0]
        .source
        .clone()
        .expect("active answer retains exact source evidence");

    let active_source = service
        .source_read(second.generation, vec![second_answer], &cancellation)
        .expect("active generation source is queryable");
    let active_text = String::from_utf8_lossy(&active_source.data.chunks[0].bytes);
    assert!(active_text.contains("43"));
    assert!(!active_text.contains("42"));

    let prior = service
        .resolve_generation(first.repository, Some(first.generation))
        .expect("prior generation remains retained");
    assert!(!prior.active);
    assert_eq!(
        service
            .resolve_generation(second.repository, None)
            .expect("active generation resolves")
            .generation,
        second.generation
    );
}

fn fixture(source: &str) -> TempDir {
    let fixture = TempDir::new().expect("fixture root exists");
    fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
    fs::write(fixture.path().join("src/lib.rs"), source).expect("fixture source writes");
    fixture
}

fn current_process_local_freshness() -> FirstSliceFreshnessStatus {
    FirstSliceFreshnessStatus {
        structural: FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan,
        semantic: FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan,
        publication: FirstSlicePublicationMode::ProcessLocalSingleStage,
        two_stage: FirstSliceTwoStageAvailability::UnavailableWithoutDurablePublication,
    }
}

fn assert_dependency_directed_rebuild(evidence: &FirstSliceIncrementalEvidence) {
    assert_eq!(
        evidence.strategy(),
        FirstSliceBuildStrategy::DependencyDirected
    );
    assert_eq!(evidence.fallback_reason(), None);
    assert_eq!(
        evidence.invalidated_domains(),
        &[
            FactDomain::Syntax,
            FactDomain::PublicSurface,
            FactDomain::Body,
            FactDomain::Tests,
            FactDomain::Services,
        ]
    );
    assert_eq!(evidence.invalidated_units(), 1);
    assert_eq!(evidence.parsed_files(), 1);
    assert_eq!(evidence.reused_parser_artifacts(), 0);
    assert_eq!(evidence.lowered_files(), 1);
    assert!(evidence.structural_cache_retained());
    assert!(evidence.trace_entries() > 0);
    assert_eq!(
        evidence.trace_entries(),
        u64::try_from(evidence.invalidation_trace().len()).expect("bounded trace length fits u64")
    );
}

fn input_change_count(evidence: &FirstSliceIncrementalEvidence, class: ChangeClass) -> u64 {
    evidence
        .input_changes()
        .iter()
        .find(|count| count.class() == class)
        .map_or(0, |count| count.inputs())
}

fn file_change_count(evidence: &FirstSliceIncrementalEvidence, kind: FileChangeKind) -> u64 {
    evidence
        .file_changes()
        .iter()
        .find(|count| count.kind() == kind)
        .map_or(0, |count| count.files())
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("test deadline is representable"),
    )
}

#[test]
fn repository_list_and_status_report_the_active_generation() {
    let fixture = TempDir::new().expect("fixture root exists");
    fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
    fs::write(fixture.path().join("src/lib.rs"), BEFORE).expect("fixture source writes");
    let cancellation = deadline();
    let mut service = FirstSliceService::new(4).expect("first-slice service initializes");

    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

    let list = service.list_repositories();
    assert_eq!(list.len(), 1);
    let entry = &list[0];
    assert_eq!(entry.repository, indexed.repository);
    assert_eq!(entry.active_generation, indexed.generation);
    assert_eq!(entry.languages, vec!["rust".to_owned()]);
    assert_eq!(entry.state, "ready");
    assert_eq!(entry.structural_freshness, "current");

    let status = service
        .repository_status(indexed.repository, None)
        .expect("known repository reports status");
    assert_eq!(status.repository, indexed.repository);
    assert_eq!(status.resolved_generation, indexed.generation);
    assert_eq!(status.active_generation, indexed.generation);
    assert_eq!(status.active_parent_generation, None);
    assert_eq!(status.state, "ready");
    assert_eq!(status.structural_freshness, "current");
    assert_eq!(status.coverage.len(), 1);
    assert_eq!(status.coverage[0].language, "rust");
    assert_eq!(status.coverage[0].indexed_files, 1);

    let catalog_page = service
        .repository_catalog_page(
            CatalogPageRequest::new(
                None,
                None,
                CatalogListFilter::new(None, None, None).expect("catalog filter is valid"),
                CatalogPageSize::new(20).expect("catalog page size is valid"),
            )
            .expect("catalog request is valid"),
            CatalogInstant::from_millis(1_000),
        )
        .expect("catalog page succeeds");
    assert_eq!(catalog_page.total_count(), 1);
    let catalog_entry = &catalog_page.items()[0];
    assert_eq!(catalog_entry.repository(), indexed.repository);
    assert_eq!(
        catalog_entry.display_name(),
        fixture
            .path()
            .file_name()
            .expect("temporary root has a basename")
            .to_string_lossy()
    );
    assert_eq!(catalog_entry.alias(), None);
    assert_eq!(catalog_entry.active_generation(), Some(indexed.generation));
    assert_eq!(catalog_entry.generation_count(), 1);
    assert_eq!(catalog_entry.state(), CatalogRepositoryState::Ready);
    assert_eq!(catalog_entry.languages().collect::<Vec<_>>(), vec!["rust"]);

    let unknown = RepositoryId::from_bytes([250; 16]);
    assert!(matches!(
        service.repository_status(unknown, None),
        Err(FirstSliceError::RepositoryNotFound)
    ));
}

#[test]
fn repository_status_keeps_exact_resolution_when_active_advances() {
    let repository_fixture = fixture(BEFORE);
    let other_fixture = fixture(OTHER);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(4).expect("first-slice service initializes");

    let first = service
        .index_rust_fixture(repository_fixture.path(), &cancellation)
        .expect("first generation indexes");
    fs::write(repository_fixture.path().join("src/lib.rs"), AFTER)
        .expect("successor source writes");
    let second = service
        .index_rust_fixture(repository_fixture.path(), &cancellation)
        .expect("successor generation indexes");

    let exact = service
        .repository_status(first.repository, Some(first.generation))
        .expect("retained exact generation reports status");
    assert_eq!(exact.resolved_generation, first.generation);
    assert_eq!(exact.active_generation, second.generation);
    assert_eq!(exact.parent_generation, None);
    assert_eq!(exact.active_parent_generation, Some(first.generation));
    assert_eq!(exact.structural_freshness, "superseded");
    assert_eq!(exact.semantic_freshness, "superseded");

    let missing = GenerationId::from_bytes([0x7f; 20]);
    assert!(matches!(
        service.repository_status(first.repository, Some(missing)),
        Err(FirstSliceError::GenerationNotFound)
    ));

    let other = service
        .index_rust_fixture(other_fixture.path(), &cancellation)
        .expect("other repository generation indexes");
    assert!(matches!(
        service.repository_status(first.repository, Some(other.generation)),
        Err(FirstSliceError::GenerationMismatch)
    ));
}

#[test]
fn symbol_relationships_returns_a_resolved_rust_call() {
    let source =
        "pub fn callee() -> u32 {\n    42\n}\n\npub fn caller() -> u32 {\n    callee()\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");
    let caller = service
        .code_locate(
            indexed.generation,
            "caller".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate caller")
        .data
        .hits
        .into_iter()
        .next()
        .expect("caller is located")
        .symbol;
    let callee = service
        .code_locate(
            indexed.generation,
            "callee".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate callee")
        .data
        .hits
        .into_iter()
        .next()
        .expect("callee is located")
        .symbol;

    let relationships = service
        .symbol_relationships(
            indexed.generation,
            BTreeSet::from([caller]),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            100,
            0,
            &cancellation,
        )
        .expect("symbol relationships query succeeds");

    assert!(!relationships.data.exact);
    assert!(!relationships.data.truncated);
    assert_eq!(relationships.data.returned_edges, 1);
    assert_eq!(relationships.data.total_edges, 1);
    assert_eq!(relationships.data.groups.len(), 1);
    let group = &relationships.data.groups[0];
    assert_eq!(group.seed, caller);
    assert_eq!(group.family, RelationFamily::Calls);
    assert_eq!(group.direction, RelationDirection::Outbound);
    assert_eq!(group.total_count, 1);
    assert_eq!(group.items.len(), 1);
    assert_eq!(group.items[0].symbol, callee);
    assert!(group.items[0].confidence >= 900);
    assert!(!group.items[0].source_refs.is_empty());
    assert_eq!(
        relationships.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
}

#[test]
fn flow_trace_returns_a_resolved_rust_call_path() {
    let source =
        "pub fn callee() -> u32 {\n    42\n}\n\npub fn caller() -> u32 {\n    callee()\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");
    let caller = service
        .code_locate(
            indexed.generation,
            "caller".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate caller")
        .data
        .hits
        .into_iter()
        .next()
        .expect("caller is located")
        .symbol;
    let callee = service
        .code_locate(
            indexed.generation,
            "callee".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate callee")
        .data
        .hits
        .into_iter()
        .next()
        .expect("callee is located")
        .symbol;

    let trace = service
        .flow_trace(
            indexed.generation,
            caller,
            None,
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            3,
            10,
            &cancellation,
        )
        .expect("flow trace query succeeds");

    assert_eq!(trace.data.paths.len(), 1);
    assert_eq!(trace.data.paths[0].nodes, vec![caller, callee]);
    assert_eq!(trace.data.paths[0].edges.len(), 1);
    assert_eq!(trace.data.paths[0].edges[0].family, RelationFamily::Calls);
    assert!(trace.data.paths[0].edges[0].confidence >= 900);
    assert!(!trace.data.paths[0].edges[0].source_refs.is_empty());
    assert!(!trace.data.paths[0].cyclic);
    assert_eq!(trace.data.frontier.reached_nodes, 2);
    assert_eq!(trace.data.frontier.examined_edges, 1);
    assert!(!trace.data.frontier.truncated);
    assert_eq!(trace.data.frontier.unresolved_boundaries, 0);
    assert_eq!(trace.data.projection.families, vec![RelationFamily::Calls]);
    assert_eq!(trace.data.projection.min_confidence, 0);
    assert_eq!(
        trace.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
}

#[test]
fn flow_trace_resolves_a_multifile_rust_scoped_call_path() {
    let fixture = TempDir::new().expect("fixture root exists");
    fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub mod gateway;\npub mod service;\npub mod worker;\n",
    )
    .expect("module root writes");
    fs::write(
        fixture.path().join("src/gateway.rs"),
        "pub fn submit_budget_request(value: usize) -> usize {\n    crate::worker::handle_budget_message(value)\n}\n",
    )
    .expect("gateway source writes");
    fs::write(
        fixture.path().join("src/service.rs"),
        "pub fn transform(value: usize) -> usize { value }\n",
    )
    .expect("service source writes");
    fs::write(
        fixture.path().join("src/worker.rs"),
        "pub fn handle_budget_message(value: usize) -> usize {\n    crate::service::transform(value)\n}\n",
    )
    .expect("worker source writes");
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("multifile fixture indexes");
    let locate = |name: &str| {
        service
            .code_locate(
                indexed.generation,
                name.to_owned(),
                LocateMode::Exact,
                8,
                0,
                &cancellation,
            )
            .expect("fixture symbol locates")
            .data
            .hits
            .into_iter()
            .next()
            .expect("fixture symbol is present")
            .symbol
    };
    let gateway = locate("submit_budget_request");
    let worker = locate("handle_budget_message");
    let transform = locate("transform");

    let trace = service
        .flow_trace(
            indexed.generation,
            gateway,
            Some(transform),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            5,
            20,
            &cancellation,
        )
        .expect("multifile flow trace succeeds");

    assert_eq!(trace.data.paths.len(), 1);
    assert_eq!(trace.data.paths[0].nodes, vec![gateway, worker, transform]);
    assert_eq!(trace.data.paths[0].edges.len(), 2);
    assert!(
        trace.data.paths[0]
            .edges
            .iter()
            .all(|edge| edge.family == RelationFamily::Calls)
    );
    assert!(
        trace.data.paths[0]
            .edges
            .iter()
            .all(|edge| !edge.source_refs.is_empty())
    );
}

#[test]
fn architecture_cycles_reports_an_honest_empty_result_for_a_known_fixture() {
    // The first-slice oracle records a direct call as a `DispatchCandidate`
    // occurrence and structural containment as a file-to-entity `Contains`
    // relation. Neither predicate belongs to a served relation family, so an
    // honest `architecture.cycles` over the fixture reports no fabricated
    // components, cycles, or break candidates while still proving the
    // generation-pinned query path, the echoed projection, and mandatory trust
    // labeling.
    let source =
        "pub fn callee() -> u32 {\n    42\n}\n\npub fn caller() -> u32 {\n    callee()\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

    let cycles = service
        .architecture_cycles(
            indexed.generation,
            vec![
                RelationFamily::Calls,
                RelationFamily::CalledBy,
                RelationFamily::References,
                RelationFamily::Types,
                RelationFamily::Implements,
                RelationFamily::Imports,
            ],
            2,
            50,
            false,
            &cancellation,
        )
        .expect("architecture cycles query succeeds");

    // No served family yields an entity-to-entity edge for this fixture, so no
    // component, cycle, or break candidate is fabricated.
    assert!(cycles.data.components.is_empty());
    assert!(cycles.data.cycles.is_empty());
    assert!(cycles.data.break_candidates.is_empty());
    assert_eq!(
        cycles.data.projection.families,
        vec![
            RelationFamily::Calls,
            RelationFamily::CalledBy,
            RelationFamily::References,
            RelationFamily::Types,
            RelationFamily::Implements,
            RelationFamily::Imports,
        ]
    );
    assert_eq!(cycles.data.projection.min_confidence, 0);
    assert_eq!(
        cycles.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
}

#[test]
fn code_dead_includes_an_isolated_rust_symbol() {
    let source = "pub fn callee() -> u32 {\n    42\n}\n\npub fn caller() -> u32 {\n    callee()\n}\n\nfn isolated() -> u32 {\n    7\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");
    let isolated = service
        .code_locate(
            indexed.generation,
            "isolated".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate isolated symbol")
        .data
        .hits[0]
        .symbol;

    let dead = service
        .code_dead(
            indexed.generation,
            CodeDeadEntryPointPolicy::Standard,
            false,
            false,
            0,
            50,
            &cancellation,
        )
        .expect("code dead query succeeds");

    assert_eq!(
        dead.data.entry_points.policy,
        CodeDeadEntryPointPolicy::Standard
    );
    assert!(!dead.data.entry_points.complete);
    assert!(!dead.data.blind_spots.is_empty());
    assert!(!dead.data.suppression_rules.is_empty());
    let isolated_candidate = dead
        .data
        .candidates
        .iter()
        .find(|candidate| candidate.symbol_id == isolated)
        .expect("an isolated entity remains visible to dead-code analysis");
    assert!(
        isolated_candidate
            .why
            .contains(&"no_incoming_references".to_owned())
    );
    assert_eq!(isolated_candidate.confidence, 300);
    let mut last_candidate = None;
    for candidate in &dead.data.candidates {
        if let Some((previous_confidence, previous_symbol)) = last_candidate {
            assert!(
                previous_confidence > candidate.confidence
                    || (previous_confidence == candidate.confidence
                        && previous_symbol <= candidate.symbol_id)
            );
        }
        last_candidate = Some((candidate.confidence, candidate.symbol_id));
        assert!(
            candidate
                .why
                .contains(&"not_observed_from_partial_entry_points".to_owned())
        );
        assert!(candidate.confidence >= 1 && candidate.confidence <= 1_000);
        assert!(!candidate.suppressions_checked.is_empty());
        assert!(candidate.source_refs.len() <= 8);
    }
    assert_eq!(
        dead.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
}

#[test]
fn architecture_overview_reports_an_honest_file_granularity_result_for_a_known_fixture() {
    // The first-slice oracle records structural containment as a file-to-entity
    // `Contains` relation and a direct call as a `DispatchCandidate` occurrence.
    // Containment yields a genuine file-granularity component model, but no
    // served relation family yields an entity-to-entity edge for this fixture,
    // so an honest `architecture.overview` reports components per file with
    // their contained symbol counts and no fabricated connections, hotspots, or
    // service/module structure, while still proving the generation-pinned query
    // path, derived-view metadata, and mandatory trust labeling.
    let source =
        "pub fn callee() -> u32 {\n    42\n}\n\npub fn caller() -> u32 {\n    callee()\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

    let overview = service
        .architecture_overview(
            indexed.generation,
            vec![ArchitectureOverviewView::Hotspots],
            0,
            50,
            true,
            &cancellation,
        )
        .expect("architecture overview query succeeds");

    // Containment gives at least one file-granularity component; every component
    // is a well-formed file component with a nonzero contained symbol count.
    assert!(!overview.data.components.is_empty());
    for component in &overview.data.components {
        assert_eq!(component.kind, "file");
        assert!(!component.id.is_empty());
        assert!(!component.name.is_empty());
        assert!(component.symbol_count >= 1);
        assert!(component.confidence <= 1_000);
        assert!(!component.responsibility_evidence.is_empty());
        assert!(component.responsibility_evidence.len() <= 16);
    }
    // No served relation family yields an entity-to-entity edge for this
    // fixture, so no connection or hotspot is fabricated.
    assert!(overview.data.connections.is_empty());
    assert!(overview.data.hotspots.is_empty());
    // The requested hotspot derived view is reported honestly.
    assert_eq!(overview.data.views.len(), 1);
    assert_eq!(
        overview.data.views[0].view,
        ArchitectureOverviewView::Hotspots
    );
    assert!(!overview.data.views[0].algorithm_version.is_empty());
    assert_eq!(
        overview.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
}

#[test]
fn tests_select_returns_a_direct_rust_test() {
    let source = "pub fn callee() -> u32 {\n    42\n}\n\npub fn caller() -> u32 {\n    callee()\n}\n\n#[test]\nfn caller_works() {\n    assert_eq!(caller(), 42);\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

    let located = service
        .code_locate(
            indexed.generation,
            "caller".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate query succeeds");
    assert_eq!(located.data.hits.len(), 1);
    let seed = located.data.hits[0].symbol;
    let test = service
        .code_locate(
            indexed.generation,
            "caller_works".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate test")
        .data
        .hits[0]
        .symbol;

    let selection = service
        .tests_select(
            indexed.generation,
            BTreeSet::from([seed]),
            Vec::new(),
            20,
            false,
            &cancellation,
        )
        .expect("tests select query succeeds");

    assert_eq!(selection.data.tests.len(), 1);
    assert_eq!(selection.data.tests[0].test_id, test);
    assert!(
        selection.data.tests[0]
            .why
            .contains(&"direct_test_edge".to_owned())
    );
    assert!(
        selection.data.tests[0]
            .why
            .iter()
            .any(|reason| reason.starts_with("via:"))
    );
    assert!(selection.data.coverage_strategy.direct_edges);
    assert!(!selection.data.coverage_strategy.transitive_signals);
    assert!(!selection.data.coverage_strategy.history_signals);
    assert!(selection.data.coverage_strategy.file_colocation_signals);
    assert!(selection.data.gaps.iter().any(|gap| {
        gap.scope == "history_evidence" && gap.reason == "history_signal_unavailable"
    }));
    assert!(selection.data.gaps.iter().any(|gap| {
        gap.scope == "runtime_evidence" && gap.reason == "runtime_coverage_unavailable"
    }));
    assert_eq!(
        selection.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
}

#[test]
fn change_impact_returns_a_resolved_rust_caller() {
    let source =
        "pub fn callee() -> u32 {\n    42\n}\n\npub fn caller() -> u32 {\n    callee()\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

    let located = service
        .code_locate(
            indexed.generation,
            "callee".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate query succeeds");
    assert_eq!(located.data.hits.len(), 1);
    let changed = located.data.hits[0].symbol;
    let caller = service
        .code_locate(
            indexed.generation,
            "caller".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate caller")
        .data
        .hits[0]
        .symbol;

    let impact = service
        .change_impact(
            indexed.generation,
            BTreeSet::from([changed]),
            Vec::new(),
            3,
            0,
            false,
            100,
            &cancellation,
        )
        .expect("change impact query succeeds");

    // The explicit symbol resolves to one honest body-classified change; the
    // lexical oracle proves no public surface for this fixture.
    assert_eq!(impact.data.resolved_changes.len(), 1);
    assert_eq!(impact.data.resolved_changes[0].symbol_id, Some(changed));
    assert_eq!(
        impact.data.resolved_changes[0].classification,
        ChangeImpactClassification::Body
    );
    assert_eq!(impact.data.impacted.len(), 1);
    assert_eq!(impact.data.impacted[0].dependents.len(), 1);
    let dependent = &impact.data.impacted[0].dependents[0];
    assert_eq!(dependent.symbol_id, caller);
    assert_eq!(dependent.distance, 1);
    assert!(dependent.confidence >= 900);
    assert_eq!(dependent.via, vec!["calls"]);
    assert!(impact.data.tests.is_empty());
    assert!(!impact.data.risk_summary.breaking_surface);
    assert_eq!(impact.data.risk_summary.fanout, 1);
    assert_eq!(impact.data.risk_summary.level, ChangeImpactRiskLevel::Low);
    assert_eq!(impact.data.risk_summary.coverage, CoverageStatus::Bounded);
    assert!(impact.data.risk_summary.dynamic_blind_spots);
    assert_eq!(
        impact.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
}

#[test]
fn change_impact_requires_an_explicit_change_set() {
    // The first slice maps only an explicit change set; an empty selector carries
    // no resolvable change and is rejected by the bounded query plan.
    let source = "pub fn answer() -> u32 {\n    42\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

    let result = service.change_impact(
        indexed.generation,
        BTreeSet::new(),
        Vec::new(),
        3,
        0,
        false,
        100,
        &cancellation,
    );
    assert!(result.is_err());
}

#[test]
fn plan_change_includes_a_resolved_rust_caller() {
    let source =
        "pub fn callee() -> u32 {\n    42\n}\n\npub fn caller() -> u32 {\n    callee()\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

    let located = service
        .code_locate(
            indexed.generation,
            "callee".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate query succeeds");
    assert_eq!(located.data.hits.len(), 1);
    let target = located.data.hits[0].symbol;
    let caller = service
        .code_locate(
            indexed.generation,
            "caller".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate caller")
        .data
        .hits[0]
        .symbol;

    let plan = service
        .plan_change(
            indexed.generation,
            PlanChangeObjective::BugFix,
            BTreeSet::from([target]),
            BTreeSet::new(),
            6,
            &cancellation,
        )
        .expect("plan change query succeeds");

    assert!(!plan.data.plan.is_empty());
    assert_eq!(plan.data.plan[0].targets, vec![target]);
    for step in &plan.data.plan {
        assert!(!step.action.is_empty());
        assert!(step.depends_on.iter().all(|dep| *dep < step.step));
    }
    assert_eq!(plan.data.affected_scope.affected_symbols, 2);
    assert_eq!(plan.data.affected_scope.affected_files, 1);
    assert!(!plan.data.affected_scope.touches_public_surface);
    assert_eq!(
        plan.data.affected_scope.risk_level,
        ChangeImpactRiskLevel::Low
    );
    // No related test entity and no fabricated open decision.
    assert!(plan.data.test_plan.is_empty());
    assert!(plan.data.open_decisions.is_empty());
    assert_eq!(
        plan.data.context_pack_request.symbols,
        [target, caller]
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(
        plan.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
}

#[test]
fn plan_change_requires_an_explicit_target_set() {
    // The first slice plans only an explicit target set; an empty selector
    // carries no resolvable target and is rejected by the bounded query plan.
    let source = "pub fn answer() -> u32 {\n    42\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

    let result = service.plan_change(
        indexed.generation,
        PlanChangeObjective::BugFix,
        BTreeSet::new(),
        BTreeSet::new(),
        6,
        &cancellation,
    );
    assert!(result.is_err());
}

#[test]
fn history_compare_reports_an_honest_empty_comparison_for_base_equal_to_head() {
    // The first-slice daemon retains few generations and maps no git ref to a
    // generation, so the honest service-level proof compares a generation
    // against itself. The two-generation load path still runs (the base document
    // is read from the generation set while the head pins the query service),
    // but diffing a document against itself yields no added, removed, or
    // modified entities, no breaking candidates, and an honest zero architecture
    // delta: every retained entity survives as an identity-preserved, non-rename
    // lineage match at full confidence, the comparison is trivially complete, and
    // mandatory trust labeling still holds. No change, rename, or architecture
    // delta is fabricated.
    let source =
        "pub fn callee() -> u32 {\n    42\n}\n\npub fn caller() -> u32 {\n    callee()\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

    let located = service
        .code_locate(
            indexed.generation,
            "callee".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .expect("locate query succeeds");
    assert_eq!(located.data.hits.len(), 1);
    let target = located.data.hits[0].symbol;

    let comparison = service
        .history_compare_with_scope_and_budget(
            indexed.generation,
            indexed.generation,
            HistoryCompareScope::default(),
            BTreeSet::new(),
            true,
            100,
            FirstSliceBudget::default(),
            &cancellation,
        )
        .expect("history compare query succeeds");

    // The resolved state pair names the same generation on both sides.
    assert_eq!(comparison.data.base_generation, indexed.generation);
    assert_eq!(comparison.data.head_generation, indexed.generation);
    assert_eq!(comparison.data.coverage, CoverageStatus::Complete);
    // No semantic change, breaking candidate, or architecture delta is fabricated.
    assert!(comparison.data.changes.is_empty());
    assert!(comparison.data.breaking_candidates.is_empty());
    assert_eq!(
        comparison.data.architecture_delta.new_cross_service_edges,
        0
    );
    assert_eq!(
        comparison
            .data
            .architecture_delta
            .removed_cross_service_edges,
        0
    );
    assert_eq!(comparison.data.architecture_delta.new_boundaries, 0);
    assert_eq!(comparison.data.architecture_delta.removed_boundaries, 0);
    // Every retained entity survives as an identity-preserved, non-rename match,
    // including the located target symbol.
    assert!(!comparison.data.lineage.is_empty());
    assert!(comparison.data.lineage.iter().all(|lineage| {
        lineage.base_symbol_id == lineage.head_symbol_id
            && !lineage.is_rename
            && lineage.confidence == 1_000
    }));
    assert!(
        comparison
            .data
            .lineage
            .iter()
            .any(|lineage| lineage.base_symbol_id == target)
    );
    assert_eq!(
        comparison.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
}

#[test]
fn history_compare_requires_a_known_base_generation() {
    // The comparison loads both generations from the bounded generation set; a
    // base generation that was never retained is rejected by the facade.
    let source = "pub fn answer() -> u32 {\n    42\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

    let unknown = GenerationId::from_bytes([0xEE; 20]);
    let result = service.history_compare(
        unknown,
        indexed.generation,
        BTreeSet::new(),
        100,
        &cancellation,
    );
    assert!(result.is_err());
}

#[test]
fn advanced_query_serves_scan_and_aggregate_operators() {
    // The service-level proof exercises both direct entity scans and relational
    // aggregation against the same indexed fixture.
    let source =
        "pub fn callee() -> u32 {\n    42\n}\n\npub fn caller() -> u32 {\n    callee()\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

    // A simple scan over functions is served from the fixture entities.
    let scan = AdvancedAstNode::Scan {
        entity: AdvancedEntityKind::Function,
        filter: None,
    };
    let result = service
        .advanced_query(
            indexed.generation,
            scan,
            false,
            100,
            0,
            ADVANCED_DEFAULT_MAX_DEPTH,
            ADVANCED_MAX_TRAVERSAL,
            None,
            &cancellation,
        )
        .expect("advanced scan query succeeds");

    // Columns are always non-empty and rows carry the fixture functions.
    assert!(!result.data.columns.is_empty());
    assert_eq!(result.data.completeness, AdvancedCompleteness::Complete);
    assert!(!result.data.rows.is_empty());
    assert_eq!(
        result.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );

    // Aggregation groups the two fixture functions by kind.
    let aggregate = AdvancedAstNode::Aggregate {
        input: Box::new(AdvancedAstNode::Scan {
            entity: AdvancedEntityKind::Function,
            filter: None,
        }),
        group_by: vec!["kind".to_owned()],
        aggregations: vec![AdvancedAggregateFunction::Count],
    };
    let aggregate = service
        .advanced_query(
            indexed.generation,
            aggregate,
            false,
            100,
            0,
            ADVANCED_DEFAULT_MAX_DEPTH,
            ADVANCED_MAX_TRAVERSAL,
            None,
            &cancellation,
        )
        .expect("advanced aggregate query succeeds");
    assert_eq!(aggregate.data.completeness, AdvancedCompleteness::Complete);
    assert!(!aggregate.data.columns.is_empty());
    assert_eq!(aggregate.data.rows.len(), 1);
    assert_eq!(
        aggregate.data.rows[0]["kind"],
        serde_json::json!("function")
    );
    assert_eq!(aggregate.data.rows[0]["count"], serde_json::json!(2));
}
