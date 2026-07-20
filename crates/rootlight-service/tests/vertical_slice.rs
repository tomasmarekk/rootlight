//! Public-boundary proof for the daemon-independent first slice.

use std::{
    fs,
    time::{Duration, Instant},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_incremental::{FactDomain, FactDomainSet};
use rootlight_query::{LocateMode, RepositoryDataTrust};
use rootlight_service::{
    ChangeClass, FileChangeKind, FirstSliceBuildStrategy, FirstSliceError,
    FirstSliceFreshnessStatus, FirstSliceIncrementalEvidence, FirstSliceObservedFreshness,
    FirstSlicePublicationMode, FirstSliceService, FirstSliceTwoStageAvailability,
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
