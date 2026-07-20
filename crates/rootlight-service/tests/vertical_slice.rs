//! Public-boundary proof for the daemon-independent first slice.

use std::{
    collections::BTreeSet,
    fs,
    time::{Duration, Instant},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_incremental::{FactDomain, FactDomainSet};
use rootlight_query::{LocateMode, RepositoryDataTrust};
use rootlight_service::{
    ChangeClass, CodeDeadEntryPointPolicy, FileChangeKind, FirstSliceBuildStrategy,
    FirstSliceError, FirstSliceFreshnessStatus, FirstSliceIncrementalEvidence,
    FirstSliceObservedFreshness, FirstSlicePublicationMode, FirstSliceService,
    FirstSliceTwoStageAvailability, RelationDirection, RelationFamily,
};
use tempfile::TempDir;

const BEFORE: &str = "pub fn answer() -> u32 {\n    42\n}\n";
const AFTER: &str = "pub fn answer() -> u32 {\n    43\n}\n";
const OTHER: &str = "pub fn other() -> u32 {\n    7\n}\n";
const KEPT: &str = "pub fn kept_after_negation() -> bool {\n    true\n}\n";
const MALFORMED: &str = "// malformed_source_sentinel\npub fn broken( {\n";

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
    assert!(source.data.chunks[0].text.contains("answer"));
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
        input_change_count(incremental_evidence, ChangeClass::Surface),
        1
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
    assert_eq!(active_source.data.chunks[0].text, AFTER);
    assert_eq!(
        active_source.data.chunks[0].content_hash,
        active_reference.content_hash()
    );

    let pinned_source = service
        .source_read(first.generation, vec![reference.clone()], &cancellation)
        .expect("superseded source snapshot remains readable");
    assert_eq!(pinned_source.data.chunks[0].text, BEFORE);
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
            &cancelled_query,
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ParentCancelled
        ))
    ));
}

#[test]
fn adapter_failure_preserves_the_prior_active_generation() {
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

    assert_eq!(
        service.index_rust_fixture(fixture.path(), &cancellation),
        Err(FirstSliceError::Adapter)
    );
    assert_eq!(
        service.active_generation_for(first.repository),
        Some(first.generation)
    );
    assert_eq!(
        service
            .resolve_generation(first.repository, None)
            .expect("prior active generation remains resolved")
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
fn rust_repository_indexes_only_extension_sources_and_preserves_lineage() {
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
    assert_eq!(first.indexed_files, 3);

    let answer = service
        .code_locate(
            first.generation,
            "answer".to_owned(),
            LocateMode::Exact,
            8,
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
    assert!(pinned_source.data.chunks[0].text.contains("42"));
    assert!(!pinned_source.data.chunks[0].text.contains("43"));

    let kept = service
        .code_locate(
            first.generation,
            "kept_after_negation".to_owned(),
            LocateMode::Exact,
            8,
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
    assert!(active_source.data.chunks[0].text.contains("43"));
    assert!(!active_source.data.chunks[0].text.contains("42"));

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
    let expected_domains = FactDomainSet::all()
        .iter()
        .filter(|domain| *domain != FactDomain::History)
        .collect::<Vec<_>>();
    assert_eq!(evidence.invalidated_domains(), expected_domains.as_slice());
    assert_eq!(evidence.invalidated_units(), 2);
    assert_eq!(evidence.parsed_files(), 1);
    assert_eq!(evidence.reused_parser_artifacts(), 0);
    assert_eq!(evidence.lowered_files(), 1);
    assert!(evidence.structural_cache_retained());
    assert!(evidence.trace_entries() > 0);
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
        .repository_status(indexed.repository)
        .expect("known repository reports status");
    assert_eq!(status.repository, indexed.repository);
    assert_eq!(status.active_generation, indexed.generation);
    assert_eq!(status.state, "ready");
    assert_eq!(status.structural_freshness, "current");
    assert_eq!(status.coverage.len(), 1);
    assert_eq!(status.coverage[0].language, "rust");
    assert_eq!(status.coverage[0].indexed_files, 1);

    let unknown = RepositoryId::from_bytes([250; 16]);
    assert!(matches!(
        service.repository_status(unknown),
        Err(FirstSliceError::RepositoryNotFound)
    ));
}

#[test]
fn symbol_relationships_reports_honest_results_for_a_known_symbol() {
    // The first-slice oracle records a direct call as a `DispatchCandidate`
    // occurrence (not a resolved `Calls` relation) and structural containment
    // as a file-to-entity `Contains` relation. Neither predicate belongs to a
    // served relation family, so an honest `symbol.relationships` expansion of
    // the caller reports no fabricated call edges while still proving the
    // generation-pinned query path, exact counts, and mandatory trust labeling.
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
            &cancellation,
        )
        .expect("locate caller")
        .data
        .hits
        .into_iter()
        .next()
        .expect("caller is located")
        .symbol;

    let relationships = service
        .symbol_relationships(
            indexed.generation,
            BTreeSet::from([caller]),
            vec![
                RelationFamily::Calls,
                RelationFamily::CalledBy,
                RelationFamily::References,
                RelationFamily::Types,
                RelationFamily::Implements,
                RelationFamily::Imports,
            ],
            Some(RelationDirection::Both),
            0,
            100,
            &cancellation,
        )
        .expect("symbol relationships query succeeds");

    // The expansion is exact and unbudgeted: every served family is honestly
    // empty for this fixture, so returned and total edge counts agree at zero
    // and no candidate or containment edge leaks into a served family.
    assert!(relationships.data.exact);
    assert!(!relationships.data.truncated);
    assert_eq!(relationships.data.returned_edges, 0);
    assert_eq!(relationships.data.total_edges, 0);
    assert!(relationships.data.groups.is_empty());
    assert_eq!(
        relationships.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
}

#[test]
fn flow_trace_reports_an_honest_empty_trace_for_a_known_symbol() {
    // The first-slice oracle records a direct call as a `DispatchCandidate`
    // occurrence and structural containment as a file-to-entity `Contains`
    // relation. Neither predicate belongs to a served relation family, so an
    // honest `flow.trace` from the caller reports no fabricated paths while
    // still proving the generation-pinned query path, the echoed projection, a
    // sane frontier bounded to the source node, and mandatory trust labeling.
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
            &cancellation,
        )
        .expect("locate caller")
        .data
        .hits
        .into_iter()
        .next()
        .expect("caller is located")
        .symbol;

    let trace = service
        .flow_trace(
            indexed.generation,
            caller,
            None,
            vec![
                RelationFamily::Calls,
                RelationFamily::CalledBy,
                RelationFamily::References,
                RelationFamily::Types,
                RelationFamily::Implements,
                RelationFamily::Imports,
            ],
            Some(RelationDirection::Both),
            0,
            3,
            10,
            &cancellation,
        )
        .expect("flow trace query succeeds");

    // The trace is exact and unbudgeted: no served family yields an
    // entity-to-entity edge for this fixture, so no path is fabricated. The
    // frontier still honestly reports the single reached source node.
    assert!(trace.data.paths.is_empty());
    assert_eq!(trace.data.frontier.reached_nodes, 1);
    assert_eq!(trace.data.frontier.examined_edges, 0);
    assert!(!trace.data.frontier.truncated);
    assert_eq!(trace.data.frontier.unresolved_boundaries, 0);
    assert_eq!(
        trace.data.projection.families,
        vec![
            RelationFamily::Calls,
            RelationFamily::CalledBy,
            RelationFamily::References,
            RelationFamily::Types,
            RelationFamily::Implements,
            RelationFamily::Imports,
        ]
    );
    assert_eq!(trace.data.projection.min_confidence, 0);
    assert_eq!(
        trace.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
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
fn code_dead_reports_an_honest_partial_result_for_a_known_fixture() {
    // The first-slice oracle records a direct call as a `DispatchCandidate`
    // occurrence and structural containment as a file-to-entity `Contains`
    // relation. Neither yields a served entity-to-entity reachability edge, so
    // an honest `code.dead` over the fixture reports no fabricated candidates
    // while still proving the generation-pinned query path, a partial
    // entry-point model, blind spots, and mandatory trust labeling.
    let source =
        "pub fn callee() -> u32 {\n    42\n}\n\npub fn caller() -> u32 {\n    callee()\n}\n";
    let fixture = fixture(source);
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
    let indexed = service
        .index_rust_fixture(fixture.path(), &cancellation)
        .expect("fixture generation indexes");

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

    // No served reachability predicate yields an entity-to-entity edge for this
    // fixture, so no dead-code candidate is fabricated.
    // The lexical oracle serves only a partial reachability graph and resolves
    // no exported entry points for this fixture, so the honest model reports a
    // partial entry-point summary and discloses blind spots rather than
    // claiming a complete dead-code verdict. Any candidate it does surface is a
    // well-formed, source-free reachability observation under that partial
    // model, not a fabricated dead-code claim.
    assert_eq!(
        dead.data.entry_points.policy,
        CodeDeadEntryPointPolicy::Standard
    );
    assert!(!dead.data.entry_points.complete);
    assert!(!dead.data.blind_spots.is_empty());
    assert!(!dead.data.suppression_rules.is_empty());
    let mut last_symbol = None;
    for candidate in &dead.data.candidates {
        // Candidates are deterministically ordered by stable symbol identity.
        if let Some(previous) = last_symbol {
            assert!(previous <= candidate.symbol_id);
        }
        last_symbol = Some(candidate.symbol_id);
        assert!(candidate
            .why
            .contains(&"unreachable_from_entry_points".to_owned()));
        assert!(candidate.confidence >= 1 && candidate.confidence <= 1_000);
        assert!(!candidate.suppressions_checked.is_empty());
        assert!(candidate.source_refs.len() <= 8);
    }
    // The first-slice entry-point model is honest about being partial.
    assert_eq!(
        dead.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
}
