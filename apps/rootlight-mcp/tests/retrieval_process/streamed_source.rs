//! Streamed source availability through actual daemon and MCP processes.
//!
//! A constant-buffer source larger than parser memory stays discoverable and
//! byte-identifiable after live replacement; source pages do not imply analysis.

use rootlight_ir::{SourceRef, SourceSpan};
use rootlight_mcp_contract::source_find::{SourceFindEnvelope, SourceFindStop};

use super::*;

const PARSER_MEMORY_BYTES: u64 = 64 * 1024 * 1024;
const TAIL: &str = "// λ😀physicaltailmarker\n";
const RAW: &[u8] = b"\xffraw";

fn record(response: &Value) {
    if let Some(path) = std::env::var_os("ROOTLIGHT_STREAMED_SOURCE_MCP_TRANSCRIPT") {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        serde_json::to_writer(&mut file, response).unwrap();
        file.write_all(b"\n").unwrap();
    }
}

fn replace_live_sources_and_restart(fixture: &mut RetrievalFixture) {
    fixture.mcp.finish();
    fixture.daemon.finish();
    fs::write(
        fixture._root.path().join("repository/raw-small.rs"),
        b"changed live bytes",
    )
    .unwrap();
    fs::write(
        fixture._root.path().join("repository/stream-large.rs"),
        b"different live bytes",
    )
    .unwrap();
    let state_dir = fixture._root.path().join("state");
    let runtime_dir = fixture._root.path().join("runtime");
    fixture.daemon = DaemonProcess::spawn(&ensure_daemon_binary(), &state_dir, &runtime_dir);
    fixture.daemon.wait_until_ready(&runtime_dir);
    fixture.mcp = McpProcess::spawn(&state_dir, &runtime_dir);
}

#[test]
fn oversized_source_default_reads_and_literal_pages_cross_real_process_boundaries() {
    let mut expected_hash = None;
    let mut fixture = RetrievalFixture::spawn_with_sources_and_prepare(
        &[] as &[(&str, &[u8])],
        FixtureLayout::Data,
        "deep",
        |root| {
            fs::write(root.join("raw-small.rs"), RAW).unwrap();
            let mut file = fs::File::create(root.join("stream-large.rs")).unwrap();
            let block = [b' '; 64 * 1024];
            let mut hash = blake3::Hasher::new();
            for _ in 0..PARSER_MEMORY_BYTES / u64::try_from(block.len()).unwrap() {
                file.write_all(&block).unwrap();
                hash.update(&block);
            }
            file.write_all(TAIL.as_bytes()).unwrap();
            hash.update(TAIL.as_bytes());
            file.sync_all().unwrap();
            expected_hash = Some(rootlight_ids::ContentHash::from_bytes(
                *hash.finalize().as_bytes(),
            ));
        },
    );
    let rejected_query = std::iter::repeat_n("alpha", 17)
        .collect::<Vec<_>>()
        .join(" ");
    let rejected = fixture.standalone(
        "query-term-admission",
        "code.locate",
        json!({"query":rejected_query,"search_modes":["lexical"]}),
    );
    record(&rejected);
    assert_public_error(&rejected, "BUDGET_EXCEEDED");
    let error = &rejected["result"]["structuredContent"]["error"];
    assert_eq!(error["details"]["resource"]["value"], "query_terms");
    assert_eq!(error["details"]["limit"]["value"], 16);
    assert_eq!(error["details"]["failure_stage"]["value"], "admission");
    assert!(!serde_json::to_string(error).unwrap().contains("alpha"));
    let located = fixture.standalone(
        "streamed-file",
        "code.locate",
        json!({
            "query":"stream-large.rs", "search_modes":["path"], "kinds":["file"],
            "scope":{"paths":["stream-large.rs"]}, "response_profile":"evidence"
        }),
    );
    record(&located);
    assert_success(&located, "code.locate");
    let output = &located["result"]["structuredContent"];
    let matches = output["data"]["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert!(matches[0]["symbol_id"].is_null());
    let full: SourceRef = serde_json::from_value(matches[0]["source_ref"].clone()).unwrap();
    assert_eq!(full.content_hash(), expected_hash.unwrap());
    assert_eq!(full.span().start_byte(), 0);
    assert_eq!(
        full.span().end_byte(),
        PARSER_MEMORY_BYTES + u64::try_from(TAIL.len()).unwrap()
    );
    assert_ne!(output["coverage"]["status"], "complete");
    let limited_text = fixture.standalone(
        "streamed-unprojected-text",
        "code.locate",
        json!({"query":"physicaltailmarker", "search_modes":["lexical"],
            "scope":{"paths":["stream-large.rs"]}}),
    );
    record(&limited_text);
    assert_public_error(&limited_text, "RESOURCE_EXHAUSTED");
    let limited = &limited_text["result"]["structuredContent"]["error"]["details"];
    assert_eq!(limited["resource"]["value"], "source_bytes");
    assert_eq!(limited["observed"]["value"], full.span().end_byte());
    assert_eq!(limited["limit"]["value"], PARSER_MEMORY_BYTES);
    let raw_path = fixture.standalone(
        "raw-file",
        "code.locate",
        json!({"query":"raw-small.rs", "search_modes":["path"], "kinds":["file"],
            "scope":{"paths":["raw-small.rs"]}, "response_profile":"evidence"}),
    );
    record(&raw_path);
    assert_success(&raw_path, "code.locate");
    let raw_matches = raw_path["result"]["structuredContent"]["data"]["matches"]
        .as_array()
        .unwrap();
    assert_eq!(raw_matches.len(), 1);
    let raw: SourceRef = serde_json::from_value(raw_matches[0]["source_ref"].clone()).unwrap();
    assert_eq!(raw.span().end_byte(), u64::try_from(RAW.len()).unwrap());
    assert_eq!(raw.content_hash(), rootlight_ids::content_hash(RAW));
    let raw_text = fixture.standalone(
        "raw-unsupported-text",
        "code.locate",
        json!({"query":"raw", "search_modes":["lexical"],
            "scope":{"paths":["raw-small.rs"]}}),
    );
    record(&raw_text);
    assert_public_error(&raw_text, "UNSUPPORTED_CAPABILITY");
    let raw_error = &raw_text["result"]["structuredContent"]["error"]["details"];
    assert_eq!(raw_error["failure_family"]["value"], "unsupported_encoding");
    assert_eq!(raw_error["failure_stage"]["value"], "admission");
    assert!(raw_error.get("limit").is_none());
    let raw_find = fixture.mcp.call(
        "raw-unsupported-literal",
        "source.find",
        json!({"repository":{"repository_id":fixture.repository_id},
            "generation":raw.generation(), "file_id":raw.span().file(), "literal":"raw"}),
    );
    record(&raw_find);
    assert_public_error(&raw_find, "UNSUPPORTED_CAPABILITY");
    assert_eq!(
        raw_find["result"]["structuredContent"]["schema_version"],
        "1.1"
    );
    let raw_find_error = &raw_find["result"]["structuredContent"]["error"]["details"];
    assert_eq!(
        raw_find_error["failure_family"]["value"],
        "unsupported_encoding"
    );
    assert_eq!(raw_find_error["failure_stage"]["value"], "admission");
    assert!(raw_find_error.get("limit").is_none());
    // Stop both processes before editing so only the original and final source
    // states can publish under the default two-generation retention policy.
    replace_live_sources_and_restart(&mut fixture);
    let arguments = json!({"root": fixture._root.path().join("repository"),
        "mode": "deep", "detached": false});
    let indexed = process_support::retry_transient_busy("streamed-reindex", |id| {
        fixture.mcp.call(id, "repo.index", arguments.clone())
    });
    record(&indexed);
    assert_success(&indexed, "repo.index");
    let data = &indexed["result"]["structuredContent"]["data"];
    assert_eq!(data["repository_id"], fixture.repository_id);
    let operation_id = data["operation_id"].as_str().unwrap();
    let replacement_generation = wait_for_publication(&mut fixture.mcp, &indexed, operation_id);
    assert_ne!(replacement_generation, full.generation().to_string());
    for (path, expected, original) in [
        ("raw-small.rs", b"changed live bytes".as_slice(), &raw),
        ("stream-large.rs", b"different live bytes".as_slice(), &full),
    ] {
        let located = fixture.standalone(
            &format!("streamed-replacement-{path}"),
            "code.locate",
            json!({"query":path, "search_modes":["path"], "kinds":["file"],
                "scope":{"paths":[path]}, "response_profile":"evidence"}),
        );
        record(&located);
        assert_success(&located, "code.locate");
        let matches = located["result"]["structuredContent"]["data"]["matches"]
            .as_array()
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0]["symbol_id"].is_null());
        let replacement: SourceRef =
            serde_json::from_value(matches[0]["source_ref"].clone()).unwrap();
        assert_eq!(replacement.repository(), original.repository());
        assert_eq!(replacement.generation().to_string(), replacement_generation);
        assert_ne!(replacement.generation(), original.generation());
        assert_eq!(replacement.span().file(), original.span().file());
        assert_eq!(replacement.span().start_byte(), 0);
        assert_eq!(
            replacement.span().end_byte(),
            u64::try_from(expected.len()).unwrap()
        );
        assert_eq!(
            replacement.content_hash(),
            rootlight_ids::content_hash(expected)
        );
    }
    let raw_bytes = fixture.standalone_version(
        "raw-exact-bytes",
        "source.read",
        json!({"references":[{"source_ref":raw}], "encoding":"bytes_base64",
            "context_lines_before":0, "context_lines_after":0, "include_line_numbers":false}),
        "1.3",
    );
    record(&raw_bytes);
    assert_success(&raw_bytes, "source.read");
    let raw_chunk = &raw_bytes["result"]["structuredContent"]["data"]["chunks"][0];
    assert_eq!(raw_chunk["source_ref"], json!(raw));
    assert_exact_base64(RAW, raw_chunk["content"].as_str().unwrap());
    let tail = SourceRef::new(
        full.repository(),
        full.generation(),
        SourceSpan::new(
            full.span().file(),
            PARSER_MEMORY_BYTES,
            full.span().end_byte(),
        )
        .unwrap(),
        full.content_hash(),
        None,
    );
    for encoding in ["bytes_base64", "lossless_text"] {
        let response = fixture.standalone_version(
            &format!("streamed-tail-{encoding}"),
            "source.read",
            json!({
                "references":[{"source_ref":tail}], "encoding":encoding,
                "context_lines_before":0, "context_lines_after":0,
                "include_line_numbers":false, "response_profile":"evidence"
            }),
            "1.3",
        );
        record(&response);
        assert_success(&response, "source.read");
        let output = &response["result"]["structuredContent"];
        let chunks = output["data"]["chunks"].as_array().unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0]["source_ref"], json!(tail));
        assert_eq!(chunks[0]["content_hash"], full.content_hash().to_string());
        assert_eq!(output["data"]["total_source_bytes"], TAIL.len());
        if encoding == "bytes_base64" {
            assert_eq!(chunks[0]["encoding"], "base64");
            assert_exact_base64(TAIL.as_bytes(), chunks[0]["content"].as_str().unwrap());
        } else {
            assert_eq!(chunks[0]["encoding"], "lossless_text");
            assert_eq!(chunks[0]["content"], TAIL);
            assert_eq!(chunks[0]["physical_encoding"], "utf-8");
        }
    }
    let mut cursor = None;
    let mut matches = Vec::new();
    for page in 0..2 {
        let mut arguments = json!({"repository":{"repository_id":fixture.repository_id},
            "generation":full.generation(), "file_id":full.span().file(),
            "literal":"λ😀physicaltailmarker", "maximum_scan_bytes":PARSER_MEMORY_BYTES});
        if let Some(cursor) = &cursor {
            arguments["cursor"] = json!(cursor);
        }
        let response = fixture
            .mcp
            .call(&format!("streamed-find-{page}"), "source.find", arguments);
        record(&response);
        assert_success(&response, "source.find");
        let envelope: SourceFindEnvelope =
            serde_json::from_value(response["result"]["structuredContent"].clone()).unwrap();
        assert_eq!(envelope.data.file.content_hash, full.content_hash());
        matches.extend(envelope.data.matches);
        cursor = envelope.next_cursor.0;
        if page == 0 {
            assert_eq!(envelope.data.stop, SourceFindStop::ScanLimit);
            assert!(envelope.truncated);
            assert!(cursor.is_some());
            assert!(matches.is_empty());
        } else {
            assert_eq!(envelope.data.stop, SourceFindStop::EndOfSource);
            assert!(!envelope.truncated);
            assert!(cursor.is_none());
        }
    }
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].span().start_byte(), PARSER_MEMORY_BYTES + 3);
    assert_eq!(matches[0].span().end_byte(), full.span().end_byte() - 1);
    assert_eq!(matches[0].content_hash(), full.content_hash());
    let explicit = fixture.standalone_version(
        "streamed-explicit-duration",
        "source.read",
        json!({
            "references":[{"source_ref":tail}], "encoding":"bytes_base64",
            "context_lines_before":0, "context_lines_after":0,
            "include_line_numbers":false, "budget":{"timeout_ms":10}
        }),
        "1.3",
    );
    record(&explicit);
    assert_public_error(&explicit, "BUSY");
    assert_eq!(
        explicit["result"]["structuredContent"]["error"]["details"]["failure_family"]["value"],
        "deadline"
    );
    fixture.finish();
}
