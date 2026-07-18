//! Public-boundary proof for the daemon-independent first slice.

use std::{
    fs,
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_query::{LocateMode, RepositoryDataTrust};
use rootlight_service::FirstSliceService;
use tempfile::TempDir;

const BEFORE: &str = "pub fn answer() -> u32 {\n    42\n}\n";
const AFTER: &str = "pub fn answer() -> u32 {\n    43\n}\n";

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

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("test deadline is representable"),
    )
}
