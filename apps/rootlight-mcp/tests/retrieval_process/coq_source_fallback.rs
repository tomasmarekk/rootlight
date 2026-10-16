//! Bounded Coq structure and source retrieval across daemon and MCP processes.
//! Written declarations retain physical evidence while proof checking and name
//! resolution remain incomplete; scan exhaustion never implies completeness.

use std::collections::BTreeMap;

use rootlight_mcp_contract::source_find::{SourceFindEnvelope, SourceFindStop};

use super::*;

const SOURCE: &str = "(* λ😀λ😀λ *)\r\nDefinition written_value : nat := 1.\r\nLemma statement : True. Proof. exact I. Qed.\r\n";
const LITERAL: &str = "λ😀λ";

fn encoded(text: &str, encoding: &str, include_bom: bool) -> Vec<u8> {
    let mut bytes = match (encoding, include_bom) {
        ("utf-8", false) => Vec::new(),
        ("utf-8", true) => vec![0xef, 0xbb, 0xbf],
        ("utf-16le", true) => vec![0xff, 0xfe],
        ("utf-16be", true) => vec![0xfe, 0xff],
        ("utf-16le" | "utf-16be", false) => Vec::new(),
        _ => panic!("unknown physical fixture encoding"),
    };
    if encoding == "utf-8" {
        bytes.extend(text.as_bytes());
    } else {
        for unit in text.encode_utf16() {
            bytes.extend(if encoding == "utf-16le" {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
    }
    bytes
}

fn record(value: &Value) {
    if let Some(path) = std::env::var_os("ROOTLIGHT_COQ_MCP_TRANSCRIPT") {
        let mut output = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        serde_json::to_writer(&mut output, value).unwrap();
        output.write_all(b"\n").unwrap();
    }
}

fn assert_coq_analysis_gap(output: &Value) {
    let coverage = output["coverage"]["languages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|language| language["language"] == "coq")
        .expect("Coq coverage is reported separately");
    assert_ne!(coverage["status"], "complete");
    assert_coq_unsupported_warning(output);
}

fn assert_coq_unsupported_warning(output: &Value) {
    assert!(
        output["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| {
                warning["code"] == "coverage_unsupported"
                    && warning["message"]
                        .as_str()
                        .is_some_and(|message| message.ends_with("language coq"))
            })
    );
}

fn declaration_pages(
    fixture: &mut RetrievalFixture,
    full_files: &BTreeMap<String, rootlight_ir::SourceRef>,
    sources: &[(&str, Vec<u8>)],
    name: &str,
    declaration: &str,
) {
    let mut canonical = BTreeMap::new();
    for alias in ["coq", "rocq"] {
        let mut declarations = BTreeMap::new();
        let mut cursor = None;
        for page in 0..=sources.len() {
            let mut arguments = json!({"query": name, "search_modes": ["exact"],
                "kinds": ["constant"], "languages": [alias], "scope": {"paths": ["proof"]},
                "max_results": 1, "response_profile": "evidence"});
            if let Some(cursor) = &cursor {
                arguments["cursor"] = json!(cursor);
            }
            let response = fixture.standalone(
                &format!("coq-declaration-{name}-{alias}-{page}"),
                "code.locate",
                arguments,
            );
            record(&response);
            assert_success(&response, "code.locate");
            let output = &response["result"]["structuredContent"];
            assert_common_read_contract(output, &fixture.repository_id);
            assert_coq_analysis_gap(output);
            assert_eq!(
                output["data"]["query_interpretation"]["semantic_available"],
                false
            );
            let matches = output["data"]["matches"].as_array().unwrap();
            assert_eq!(matches.len(), 1);
            let found = &matches[0];
            assert_eq!(found["display_name"], name);
            assert_eq!(found["kind"], "constant");
            let symbol: SymbolId = serde_json::from_value(found["symbol_id"].clone()).unwrap();
            let reference: rootlight_ir::SourceRef =
                serde_json::from_value(found["source_ref"].clone()).unwrap();
            let path = found["path"].as_str().unwrap();
            let full = &full_files[path];
            assert_eq!(reference.repository(), full.repository());
            assert_eq!(reference.generation(), full.generation());
            assert_eq!(reference.span().file(), full.span().file());
            assert_eq!(found["file_id"], json!(full.span().file()));
            assert_eq!(reference.content_hash(), full.content_hash());
            let raw = &sources.iter().find(|(file, _)| *file == path).unwrap().1;
            let encoding = match path {
                "proof/little.v" => "utf-16le",
                "proof/big.v" => "utf-16be",
                _ => "utf-8",
            };
            let spelling = encoded(declaration, encoding, false);
            let expected: Vec<_> = raw
                .windows(spelling.len())
                .enumerate()
                .filter(|(start, bytes)| {
                    *bytes == spelling && (encoding == "utf-8" || start.is_multiple_of(2))
                })
                .map(|(start, _)| {
                    (
                        u64::try_from(start).unwrap(),
                        u64::try_from(start + spelling.len()).unwrap(),
                    )
                })
                .collect();
            assert_eq!(
                expected,
                [(reference.span().start_byte(), reference.span().end_byte())]
            );
            assert!(
                declarations
                    .insert(path.to_owned(), (symbol, reference))
                    .is_none()
            );
            cursor = output["next_cursor"].as_str().map(str::to_owned);
            assert_eq!(output["truncated"], cursor.is_some());
            assert_eq!(
                output["completeness"]["continuation"],
                if cursor.is_some() {
                    "available"
                } else {
                    "not_applicable"
                }
            );
            if cursor.is_none() {
                assert_eq!(output["completeness"]["state"], "complete");
                break;
            }
        }
        assert!(
            cursor.is_none(),
            "declaration pagination reaches exhaustion"
        );
        assert_eq!(declarations.len(), sources.len());
        if alias == "coq" {
            canonical = declarations;
        } else {
            assert_eq!(declarations, canonical);
        }
    }
}

fn source_pages(
    fixture: &mut RetrievalFixture,
    full: &rootlight_ir::SourceRef,
    raw: &[u8],
    path: &str,
    encoding: &str,
    scan_limit: bool,
) {
    let literal = encoded(LITERAL, encoding, false);
    let expected: Vec<_> = raw
        .windows(literal.len())
        .enumerate()
        .filter(|(start, window)| {
            *window == literal && (encoding == "utf-8" || start.is_multiple_of(2))
        })
        .map(|(start, _)| {
            (
                u64::try_from(start).unwrap(),
                u64::try_from(start + literal.len()).unwrap(),
            )
        })
        .collect();
    assert_eq!(expected.len(), 2, "the scalar matches overlap");
    let mut cursor = None;
    let mut position = if encoding == "utf-8" { 0 } else { 2 };
    let mut actual = Vec::new();
    let mut stops = Vec::new();
    for page in 0..=raw.len() {
        let mut arguments = json!({
            "repository": {"repository_id": fixture.repository_id},
            "generation": full.generation(), "file_id": full.span().file(),
            "literal": LITERAL,
            "maximum_matches": if scan_limit { 128 } else { 1 },
            "maximum_scan_bytes": if scan_limit { 16 } else { raw.len() },
        });
        if let Some(cursor) = &cursor {
            arguments["cursor"] = json!(cursor);
        }
        let response = fixture.mcp.call(
            &format!("coq-find-{path}-{scan_limit}-{page}"),
            "source.find",
            arguments,
        );
        record(&response);
        assert_success(&response, "source.find");
        let output = &response["result"]["structuredContent"];
        let envelope: SourceFindEnvelope = serde_json::from_value(output.clone()).unwrap();
        assert_eq!(envelope.repository.repository_id, full.repository());
        assert_eq!(envelope.generation, full.generation());
        assert_eq!(envelope.data.file.id, full.span().file());
        assert_eq!(envelope.data.file.path, path);
        assert_eq!(envelope.data.file.language, "coq");
        assert_eq!(json!(envelope.data.file.encoding), encoding);
        assert_eq!(envelope.data.file.content_hash, full.content_hash());
        assert_eq!(
            envelope.data.file.byte_length,
            u64::try_from(raw.len()).unwrap()
        );
        assert_eq!(envelope.data.scanned_start_byte, position);
        assert!(envelope.data.scanned_end_byte > position);
        position = envelope.data.scanned_end_byte;
        assert_eq!(envelope.usage.source_bytes, 0);
        assert_common_usage(output);
        for field in ["coverage", "completeness", "total_matches"] {
            assert!(output.get(field).is_none());
        }
        for reference in &envelope.data.matches {
            assert_eq!(reference.repository(), full.repository());
            assert_eq!(reference.generation(), full.generation());
            assert_eq!(reference.content_hash(), full.content_hash());
            assert_eq!(reference.span().file(), full.span().file());
            assert_eq!(
                &raw[usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap()],
                literal
            );
            actual.push((reference.span().start_byte(), reference.span().end_byte()));
        }
        stops.push(envelope.data.stop);
        cursor = envelope.next_cursor.0;
        if cursor.is_none() {
            assert!(!envelope.truncated);
            assert_eq!(envelope.data.stop, SourceFindStop::EndOfSource);
            assert_eq!(position, u64::try_from(raw.len()).unwrap());
            assert_eq!(actual, expected);
            assert!(stops.contains(&if scan_limit {
                SourceFindStop::ScanLimit
            } else {
                SourceFindStop::MatchLimit
            }));
            return;
        }
        assert!(envelope.truncated);
        assert_ne!(envelope.data.stop, SourceFindStop::EndOfSource);
    }
    panic!("every page must progress through this finite source");
}

#[test]
fn coq_source_fallback_keeps_aliases_exact_encoded_bytes_and_truthful_pages() {
    let cases = [
        ("proof/utf8.v", "utf-8", false),
        ("proof/bom.v", "utf-8", true),
        ("proof/little.v", "utf-16le", true),
        ("proof/big.v", "utf-16be", true),
    ];
    let sources: Vec<_> = cases
        .iter()
        .map(|(path, encoding, bom)| (*path, encoded(SOURCE, encoding, *bom)))
        .collect();
    record(
        &json!({"fixture_sources": sources, "scope": "bounded Coq structure and source retrieval"}),
    );
    let mut fixture = RetrievalFixture::spawn_with_sources(&sources, FixtureLayout::Data);
    let mut canonical_files = BTreeMap::new();
    for alias in ["coq", "rocq"] {
        let mut cursor = None;
        let mut files = BTreeMap::new();
        for page in 0..=sources.len() {
            let mut arguments = json!({"query": "proof/", "search_modes": ["path"],
                "kinds": ["file"], "languages": [alias], "scope": {"paths": ["proof"]},
                "max_results": 1, "response_profile": "evidence"});
            if let Some(cursor) = &cursor {
                arguments["cursor"] = json!(cursor);
            }
            let response = fixture.standalone(
                &format!("coq-files-{alias}-{page}"),
                "code.locate",
                arguments,
            );
            record(&response);
            assert_success(&response, "code.locate");
            let output = &response["result"]["structuredContent"];
            assert_common_read_contract(output, &fixture.repository_id);
            assert_coq_analysis_gap(output);
            for found in output["data"]["matches"].as_array().unwrap() {
                assert_eq!(found["kind"], "file");
                assert!(found["symbol_id"].is_null());
                let path = found["path"].as_str().unwrap().to_owned();
                let reference: rootlight_ir::SourceRef =
                    serde_json::from_value(found["source_ref"].clone()).unwrap();
                assert_eq!(found["file_id"], json!(reference.span().file()));
                assert!(files.insert(path, reference).is_none());
            }
            cursor = output["next_cursor"].as_str().map(str::to_owned);
            assert_eq!(output["truncated"], cursor.is_some());
            assert_eq!(
                output["completeness"]["continuation"],
                if cursor.is_some() {
                    "available"
                } else {
                    "not_applicable"
                }
            );
            if cursor.is_none() {
                assert_eq!(output["completeness"]["state"], "complete");
                break;
            }
        }
        assert!(cursor.is_none(), "file pagination reaches exhaustion");
        assert_eq!(files.len(), sources.len());
        if alias == "coq" {
            canonical_files = files;
        } else {
            assert_eq!(files, canonical_files);
        }
    }
    for ((path, raw), (_, encoding, _)) in sources.iter().zip(cases) {
        let full = &canonical_files[*path];
        assert_eq!(full.span().start_byte(), 0);
        assert_eq!(full.span().end_byte(), u64::try_from(raw.len()).unwrap());
        assert_eq!(full.content_hash(), rootlight_ids::content_hash(raw));
        let response = fixture.standalone_version(
            &format!("coq-raw-read-{path}"),
            "source.read",
            json!({"references": [{"source_ref": full}], "encoding": "bytes_base64",
                "context_lines_before": 0, "context_lines_after": 0,
                "include_line_numbers": false, "response_profile": "evidence"}),
            "1.3",
        );
        record(&response);
        assert_success(&response, "source.read");
        let output = &response["result"]["structuredContent"];
        let chunk = &output["data"]["chunks"][0];
        assert_eq!(output["data"]["chunks"].as_array().unwrap().len(), 1);
        assert_eq!(chunk["encoding"], "base64");
        assert_eq!(chunk["source_ref"], json!(full));
        assert_eq!(chunk["content_hash"], full.content_hash().to_string());
        assert_eq!(output["usage"]["source_bytes"], raw.len());
        assert_exact_base64(raw, chunk["content"].as_str().unwrap());
        source_pages(&mut fixture, full, raw, path, encoding, false);
        source_pages(&mut fixture, full, raw, path, encoding, true);
        let absent = fixture.mcp.call(
            &format!("coq-empty-find-{path}"),
            "source.find",
            json!({"repository": {"repository_id": fixture.repository_id},
                "generation": full.generation(), "file_id": full.span().file(),
                "literal": "absent_source_literal"}),
        );
        record(&absent);
        assert_success(&absent, "source.find");
        let absent: SourceFindEnvelope =
            serde_json::from_value(absent["result"]["structuredContent"].clone()).unwrap();
        assert!(absent.data.matches.is_empty());
        assert_eq!(absent.data.stop, SourceFindStop::EndOfSource);
        assert!(!absent.truncated);
        assert!(absent.next_cursor.0.is_none());
    }
    for (name, declaration) in [
        ("written_value", "Definition written_value : nat := 1"),
        ("statement", "Lemma statement : True"),
    ] {
        declaration_pages(&mut fixture, &canonical_files, &sources, name, declaration);
    }
    let definitions = fixture.standalone(
        "coq-absent-function-kind",
        "code.locate",
        json!({"query": "written_value", "search_modes": ["exact"],
            "kinds": ["function"], "languages": ["coq"],
            "scope": {"paths": ["proof"]}, "response_profile": "evidence"}),
    );
    record(&definitions);
    assert_success(&definitions, "code.locate");
    let output = &definitions["result"]["structuredContent"];
    assert_eq!(output["data"]["matches"], json!([]));
    assert_eq!(output["coverage"]["languages"], json!([]));
    assert_eq!(output["coverage"]["status"], "bounded");
    assert_eq!(output["coverage"]["skipped_inputs"], sources.len());
    assert_eq!(
        output["data"]["query_interpretation"]["semantic_available"],
        false
    );
    assert_coq_unsupported_warning(output);
    assert!(
        output["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| { warning["code"] == "negative_claims_inconclusive" })
    );
    fixture.finish();
}
