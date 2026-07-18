//! Process-boundary proof for the first-slice CLI demo.

use std::process::Command;

#[test]
fn cli_demo_returns_three_queries_and_retains_the_old_generation() {
    let output = Command::new(env!("CARGO_BIN_EXE_rootlight"))
        .arg("first-slice-demo")
        .output()
        .expect("CLI demo process starts");
    assert!(
        output.status.success(),
        "CLI demo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("CLI output is valid JSON");
    let data = &envelope["result"]["data"];

    assert_eq!(envelope["contract_version"], "1.0");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["type"], "first_slice_demo");
    assert_eq!(data["contract_version"], "1.0");
    assert_eq!(data["storage_mode"], "ephemeral_sqlite_and_lexical");
    assert_eq!(data["first_freshness"], "active_at_query_time");
    assert_eq!(data["retained_first_freshness"], "retained_after_update");
    assert_eq!(data["second_freshness"], "active");
    assert_eq!(data["first"]["discovered_inputs"], 1);
    assert_eq!(data["first"]["indexed_files"], 1);
    assert!(
        data["first"]["entities"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert!(
        data["first"]["oracle_allocated_bytes"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert_eq!(
        data["locate"]["data"]["hits"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        data["locate"]["data"]["hits"][0]["trust"],
        "untrusted_repository_data"
    );
    assert!(
        data["locate"]["data"]["coverage"]
            .as_array()
            .is_some_and(|coverage| !coverage.is_empty())
    );
    assert_eq!(
        data["explain"]["data"]["entity"]["id"],
        data["locate"]["data"]["hits"][0]["symbol"]
    );
    assert_eq!(
        data["source"]["data"]["chunks"][0]["trust"],
        "untrusted_repository_data"
    );
    assert!(
        data["source"]["data"]["chunks"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("answer"))
    );
    assert_eq!(data["second"]["parent"], data["first"]["generation"]);
    assert_ne!(data["second"]["generation"], data["first"]["generation"]);
    assert_eq!(
        data["second_locate"]["data"]["hits"][0]["symbol"],
        data["locate"]["data"]["hits"][0]["symbol"]
    );
    assert_eq!(data["pinned_first"]["data"], data["locate"]["data"]);
    assert_eq!(
        data["measurements"]["lexical_index_bytes"],
        serde_json::Value::Null
    );
    assert_eq!(
        data["measurements"]["lexical_index_size_status"],
        "unavailable_in_memory_backend"
    );
    assert_eq!(
        data["measurements"]["peak_rss_bytes"],
        serde_json::Value::Null
    );
    assert_eq!(
        data["measurements"]["peak_rss_status"],
        "unavailable_portable_sampler"
    );
}
