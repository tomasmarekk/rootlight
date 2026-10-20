//! Mixed diagnostic batches retain source-only readiness until a graph query.
//! The later graph request still executes complete verified durable recovery.

use super::*;

#[test]
fn source_only_reopen_delays_graph_and_keeps_later_locate_exact() {
    let storage = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let fixture = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let raw = b"// ababa\npub fn lantern() {}\n";
    std::fs::write(fixture.path().join("lib.rs"), raw).unwrap();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let cancellation = Cancellation::new();
    let (receipt, file) = {
        let mut service =
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .unwrap();
        let file = service
            .code_locate(
                receipt.generation,
                "lantern".to_owned(),
                LocateMode::Exact,
                10,
                0,
                &cancellation,
            )
            .unwrap()
            .data
            .hits[0]
            .file;
        (receipt, file)
    };
    let (mut service, repository, mut deferred) = open_service(
        &storage.path().as_os_str().to_os_string(),
        &fixture.path().as_os_str().to_os_string(),
        &cancellation,
    )
    .unwrap();
    assert_eq!(repository, receipt.repository);
    assert!(
        service
            .resolve_generation(repository, Some(receipt.generation))
            .is_err()
    );
    let request: QueryRequest = serde_json::from_value(
        json!({ "query":"aba", "mode":"source_find", "generation":receipt.generation,
        "find": {"file":file, "read_matches":true} }),
    )
    .unwrap();
    let found = query_reopened(
        &mut service,
        &mut deferred,
        repository,
        &request,
        true,
        &cancellation,
    )
    .unwrap();
    assert_eq!(found["matches"].as_array().unwrap().len(), 2);
    assert_eq!(found["source_reads"].as_array().unwrap().len(), 2);
    assert!(deferred.is_some());
    assert!(
        service
            .resolve_generation(repository, Some(receipt.generation))
            .is_err()
    );
    let request: QueryRequest =
        serde_json::from_value(json!({ "query":"lantern", "generation":receipt.generation }))
            .unwrap();
    let located = query_reopened(
        &mut service,
        &mut deferred,
        repository,
        &request,
        true,
        &cancellation,
    )
    .unwrap();
    assert!(located.is_object());
    assert!(deferred.is_none());
    assert!(
        service
            .resolve_generation(repository, Some(receipt.generation))
            .is_ok()
    );
}
