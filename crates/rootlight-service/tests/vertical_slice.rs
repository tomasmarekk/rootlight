//! Public-boundary proof for the daemon-independent first slice.

use std::{
    fs,
    time::{Duration, Instant},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_query::{LocateMode, RepositoryDataTrust};
use rootlight_service::{FirstSliceError, FirstSliceService};
use tempfile::TempDir;

const BEFORE: &str = "pub fn answer() -> u32 {\n    42\n}\n";
const AFTER: &str = "pub fn answer() -> u32 {\n    43\n}\n";
const OTHER: &str = "pub fn other() -> u32 {\n    7\n}\n";

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
        .source_read(first.generation, vec![reference], &cancellation)
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

fn fixture(source: &str) -> TempDir {
    let fixture = TempDir::new().expect("fixture root exists");
    fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
    fs::write(fixture.path().join("src/lib.rs"), source).expect("fixture source writes");
    fixture
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("test deadline is representable"),
    )
}
