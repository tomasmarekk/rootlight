//! Real-process context prefixes with unchanged original source authority.
//!
//! Reuses the retrieval harness; this is debug transport proof, not installed acceptance.

use super::*;

fn encode(text: &str, encoding: &str) -> Vec<u8> {
    match encoding {
        "utf-8" => text.as_bytes().to_vec(),
        "utf-16le" => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        "utf-16be" => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => unreachable!("fixture encoding is closed"),
    }
}

#[test]
fn context_pack_lossless_prefixes_cross_real_process_boundaries() {
    // Deseret letters are valid non-BMP identifier characters. A long header
    // independently exercises signature clipping before the long string body.
    let declaration = format!(
        "pub fn process_prefix_target(value_{}: usize) -> &'static str",
        "\u{10400}".repeat(320),
    );
    let body = format!(
        "{declaration} {{\r\n    \"{}\"\r\n}}",
        "\u{1f642}\u{2603}".repeat(400)
    );
    let text = format!("\u{feff}{body}\r\npub fn after_prefix_target() {{}}\r\n");
    assert!(declaration.len() > 1_024);
    assert!(body.len() > 3_000);
    let encodings = ["utf-8", "utf-16le", "utf-16be"];
    let paths = ["prefix-utf8.rs", "prefix-le.rs", "prefix-be.rs"];
    let sources: Vec<_> = paths
        .iter()
        .zip(encodings)
        .map(|(path, encoding)| (*path, encode(&text, encoding)))
        .collect();
    let mut fixture = RetrievalFixture::spawn_with_sources(&sources, FixtureLayout::Data);
    let mut original_definitions = Vec::new();
    for (ordinal, ((path, raw), encoding)) in sources.iter().zip(encodings).enumerate() {
        let located = fixture.standalone(
            &format!("prefix-process-locate-{ordinal}"),
            "code.locate",
            json!({"query": "process_prefix_target", "search_modes": ["exact"],
                "kinds": ["function"], "scope": {"paths": [path]}, "response_profile": "evidence"}),
        );
        assert_success(&located, "code.locate");
        let located_output = &located["result"]["structuredContent"];
        let matches = located_output["data"]["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        let matched = &matches[0];
        assert_eq!(matched["path"], *path);
        assert_eq!(matched["kind"], "function");
        let symbol = matched["symbol_id"].clone();
        serde_json::from_value::<SymbolId>(symbol.clone()).unwrap();
        let definition: rootlight_ir::SourceRef =
            serde_json::from_value(matched["source_ref"].clone()).unwrap();
        let start = encode("\u{feff}", encoding).len();
        let end = start + encode(&body, encoding).len();
        assert_eq!(
            definition.span().start_byte(),
            u64::try_from(start).unwrap()
        );
        assert_eq!(definition.span().end_byte(), u64::try_from(end).unwrap());
        assert_eq!(definition.content_hash(), rootlight_ids::content_hash(raw));
        assert_eq!(
            located_output["generation"]["generation_id"],
            definition.generation().to_string()
        );
        assert_eq!(
            located_output["repository"]["repository_id"],
            definition.repository().to_string()
        );
        assert_eq!(raw.get(start..end).unwrap(), encode(&body, encoding));
        for source_policy in ["focused_snippets", "signatures"] {
            // Compact intentionally suppresses snippets; Evidence uses the
            // existing focused preview ceiling without altering caller limits.
            let profile = if source_policy == "focused_snippets" {
                "evidence"
            } else {
                "compact"
            };
            // Focused snippets require a source-compatible public request.
            // The exact first-stage body may be omitted by its bounded budget;
            // second-stage presentation must still use the full definition ref.
            let sections = if source_policy == "focused_snippets" {
                vec!["definitions", "source"]
            } else {
                vec!["definitions"]
            };
            let pack = fixture.mcp.call(
                &format!("prefix-process-pack-{ordinal}-{source_policy}"),
                "context.pack",
                json!({"repository": {"repository_id": fixture.repository_id},
                    "generation": definition.generation(), "task": "explain process_prefix_target",
                    "seeds": {"symbols": [symbol]}, "sections": sections,
                    "source_policy": source_policy, "token_budget": 20_000,
                    "response_profile": profile}),
            );
            assert_success(&pack, "context.pack");
            let output = &pack["result"]["structuredContent"];
            assert_common_read_contract(output, &fixture.repository_id);
            assert_eq!(
                output["generation"]["generation_id"],
                definition.generation().to_string()
            );
            assert!(output["usage"]["estimated_tokens"].as_u64().unwrap() <= 20_000);
            serde_json::from_value::<rootlight_mcp_contract::context::ContextPackOutputV1_1>(
                output.clone(),
            )
            .unwrap();
            let item = output["data"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["role"] == "definition" && item["symbol_id"] == symbol)
                .unwrap();
            assert_eq!(
                item["source_ref"],
                serde_json::to_value(&definition).unwrap()
            );
            if source_policy == "focused_snippets" {
                assert!(item.get("signature").is_none());
                let snippet = &item["snippet"];
                assert_eq!(
                    snippet["source_ref"],
                    serde_json::to_value(&definition).unwrap()
                );
                assert_eq!(snippet["truncated"], true);
                assert_eq!(snippet["provenance"], "source_read");
                assert_eq!(snippet["trust"], "untrusted_repository_data");
                let presented = snippet["content"].as_str().unwrap();
                assert!(!presented.is_empty());
                assert!(presented.len() <= 2_048);
                assert!(presented.len() < body.len());
                assert!(presented.contains('\u{10400}'));
                assert!(text.starts_with(presented));
                assert!(raw.starts_with(&encode(presented, encoding)));
                assert!(
                    output["usage"]["source_bytes"].as_u64().unwrap()
                        >= u64::try_from(encode(presented, encoding).len()).unwrap()
                );
                assert!(
                    output["data"]["omitted"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|omission| omission["reason"] == "source_truncated")
                );
            } else {
                assert!(item.get("snippet").is_none());
                let signature = item["signature"].as_str().unwrap();
                assert!(!signature.is_empty());
                assert!(signature.len() <= 1_024);
                assert!(signature.contains('\u{10400}'));
                assert!(declaration.starts_with(signature));
                assert!(signature.len() < declaration.len());
            }
        }
        let arguments = json!({"references": [{"source_ref": definition}],
            "context_lines_before": 0, "context_lines_after": 0,
            "include_line_numbers": false, "response_profile": "evidence"});
        assert_legacy_encoded_process_read(
            &mut fixture,
            &arguments,
            &definition,
            raw,
            path,
            ordinal,
            "context-definition",
        );
        original_definitions.push(definition);
    }
    for ((path, _), encoding) in sources.iter().zip(encodings) {
        fs::write(
            fixture._root.path().join("repository").join(path),
            encode(
                "\u{feff}pub fn replacement_prefix_target() {}\r\n",
                encoding,
            ),
        )
        .unwrap();
    }
    let arguments =
        json!({"root": fixture._root.path().join("repository"), "mode": "auto", "detached": false});
    let indexed = process_support::retry_transient_busy("prefix-process-reindex", |id| {
        fixture.mcp.call(id, "repo.index", arguments.clone())
    });
    assert_success(&indexed, "repo.index");
    let operation = indexed["result"]["structuredContent"]["data"]["operation_id"]
        .as_str()
        .unwrap();
    wait_for_publication(&mut fixture.mcp, &indexed, operation);
    let current = fixture.standalone("prefix-process-current", "code.locate",
        json!({"query": "replacement_prefix_target", "search_modes": ["exact"], "kinds": ["function"],
            "scope": {"paths": [paths[0]]}, "response_profile": "evidence"}));
    assert_success(&current, "code.locate");
    let current = &current["result"]["structuredContent"];
    assert_eq!(current["data"]["matches"].as_array().unwrap().len(), 1);
    assert_ne!(
        current["generation"]["generation_id"],
        original_definitions[0].generation().to_string()
    );
    for (ordinal, (((path, _), encoding), definition)) in sources
        .iter()
        .zip(encodings)
        .zip(&original_definitions)
        .enumerate()
    {
        let read = fixture.mcp.call_version(
            &format!("prefix-process-exact-retained-{ordinal}"),
            "source.read",
            json!({"repository": {"repository_id": fixture.repository_id},
                "generation": definition.generation(), "references": [{"source_ref": definition}],
                "context_lines_before": 0, "context_lines_after": 0,
                "include_line_numbers": false, "response_profile": "evidence"}),
            "1.3",
        );
        assert_lossless_process_read(&read, definition, path, encoding, &body);
    }
    fixture.finish();
}
