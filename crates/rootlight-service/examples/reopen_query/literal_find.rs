//! Exact known-file literal queries for the durable reopen diagnostic.
//! Results retain the public service's real scan boundaries and cursor parts.
//! Matches stay source references; explicitly requested reads remain separate.
//! Memory counters describe the literal result, excluding those separate reads.

use std::error::Error;

use rootlight_cancel::Cancellation;
use rootlight_ids::{FileId, GenerationId, RepositoryId};
use rootlight_service::{FirstSliceBudget, FirstSliceService, SourceEncoding, SourceReadOptions};
use rootlight_source::{SourceFindBudget, SourceFindCursor, SourceFindStop, SourceTrust};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::QueryRequest;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
/// A generation-owned file and optional verified continuation parts.
pub(super) struct FindRequest {
    pub(super) file: FileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<CursorParts>,
    #[serde(default)]
    pub(super) read_matches: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CursorParts {
    next_byte: usize,
    matched_prefix: usize,
}

/// Rejects lexical selectors and preserves the source matcher's query admission.
///
/// # Errors
/// Returns a static reason for an absent file, invalid query or mixed selectors.
pub(super) fn validate(request: &QueryRequest) -> Result<(), &'static str> {
    if request.find.is_none() {
        return Err("source_find requires an immutable file selector");
    }
    if request.query.is_empty()
        || request.query.len() > SourceFindBudget::new().matcher.max_query_bytes
    {
        return Err("literal query exceeds source matcher admission");
    }
    if request.offset != 0 || request.path_prefix.is_some() || request.paged_source {
        return Err("source_find uses file and cursor, not lexical lookup selectors");
    }
    Ok(())
}

/// Executes a literal page and any explicitly requested exact match reads.
///
/// # Errors
/// Preserves service/source failures and rejects inconsistent response identity.
pub(super) fn query(
    service: &FirstSliceService,
    repository: RepositoryId,
    generation: GenerationId,
    request: &QueryRequest,
    cancellation: &Cancellation,
) -> Result<Value, Box<dyn Error>> {
    validate(request)?;
    let selector = request.find.as_ref().ok_or("missing literal selector")?;
    let cursor = selector.cursor.map(|parts| SourceFindCursor {
        next_byte: parts.next_byte,
        matched_prefix: parts.matched_prefix,
    });
    let found = service.find_literal(
        generation,
        selector.file,
        &request.query,
        cursor,
        SourceFindBudget::new(),
        cancellation,
    )?;
    if found.generation != generation || found.file.id != selector.file {
        return Err("literal search returned another generation or file".into());
    }
    let trust = match found.file.trust {
        SourceTrust::UntrustedRepositoryData => "untrusted_repository_data",
        _ => return Err("literal search returned an unsupported trust marker".into()),
    };
    let mut source_reads = Vec::new();
    if selector.read_matches {
        for reference in &found.matches {
            let read = service.source_read_with_options_and_budget(
                generation,
                vec![reference.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                cancellation,
            )?;
            let [chunk] = read.data.chunks.as_slice() else {
                return Err("literal match read returned an unexpected chunk count".into());
            };
            if read.data.generation != generation || &chunk.reference != reference {
                return Err("literal match read returned another generation or source".into());
            }
            source_reads.push(read);
        }
    }
    let stop = match found.stop {
        SourceFindStop::EndOfSource => "end_of_source",
        SourceFindStop::ScanLimit => "scan_limit",
        SourceFindStop::MatchLimit => "match_limit",
        SourceFindStop::MatcherMemoryLimit => "matcher_memory_limit",
        SourceFindStop::ResponseMemoryLimit => "response_memory_limit",
    };
    // These are verified service cursor parts, not an authenticated MCP token.
    // The transport contract is qualified separately by the daemon/MCP tests.
    let continuation = found.continuation.map(|parts| CursorParts {
        next_byte: parts.next_byte,
        matched_prefix: parts.matched_prefix,
    });
    Ok(json!({
        "schema":"rootlight-reopen-source-find-diagnostic/1",
        "repository":repository,
        "generation":found.generation,
        "query":request.query,
        "file":{
            "id":found.file.id,
            "path":found.file.path,
            "language":found.file.language,
            "content_hash":found.file.content_hash,
            "byte_length":found.file.byte_length,
            "encoding":found.file.encoding.as_str(),
            "generated":found.file.generated,
            "trust":trust,
            "extracted_from":found.file.extracted_from,
        },
        "matches":found.matches,
        "scanned":{"start_byte":found.scanned.start,"end_byte":found.scanned.end},
        "stop":stop,
        "continuation":continuation,
        "restoration_bytes":found.restoration_bytes,
        "total_metadata_bytes":found.total_metadata_bytes,
        "total_response_memory_bytes":found.total_response_memory_bytes,
        "source_reads":source_reads,
        "full_semantic_completeness_proven":false,
        "installed_mcp_qualified":false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{QueryMode, next_batch_request, query_existing};

    fn parse(value: Value) -> Result<QueryRequest, Box<dyn Error>> {
        let mut input = std::io::Cursor::new(serde_json::to_vec(&value)?);
        next_batch_request(&mut input, &mut Vec::new())?.ok_or_else(|| "empty request".into())
    }

    #[test]
    fn literal_requests_keep_admission_and_legacy_serialization_distinct() {
        let file = FileId::from_bytes([3; 20]);
        let request =
            parse(json!({"mode":"source_find","query":"x".repeat(129),"find":{"file":file}}))
                .unwrap();
        assert_eq!(request.mode, QueryMode::SourceFind);
        let legacy = parse(json!({"query":"anchor"})).unwrap();
        assert!(serde_json::to_value(legacy).unwrap().get("find").is_none());
        for invalid in [
            json!({"mode":"source_find","query":"x"}),
            json!({"mode":"source_find","query":"","find":{"file":file}}),
            json!({"mode":"source_find","query":"x","find":{"file":file},"offset":1}),
            json!({"mode":"source_find","query":"x","find":{"file":file},"path_prefix":"file.rs"}),
            json!({"mode":"source_find","query":"x","find":{"file":file},"paged_source":true}),
            json!({"query":"x","find":{"file":file}}),
            json!({"mode":"source_find","query":"x","find":{"file":file,"cursor":{"next_byte":1,"matched_prefix":0,"extra":true}}}),
        ] {
            assert!(parse(invalid).is_err());
        }
    }

    #[test]
    fn complete_maximum_escaped_literal_fits_the_batch_transport() {
        let file = FileId::from_bytes([3; 20]);
        let query = "\0".repeat(SourceFindBudget::new().matcher.max_query_bytes);
        let value = json!({"mode":"source_find","query":query,"find":{"file":file}});
        assert!(serde_json::to_vec(&value).unwrap().len() > 64 * 1024);
        assert_eq!(parse(value).unwrap().query, query);
        assert!(
            parse(
                json!({"mode":"source_find","query":"x".repeat(query.len()+1),"find":{"file":file}})
            )
            .is_err()
        );
    }

    #[test]
    fn literal_queries_preserve_original_bytes_overlaps_and_retained_generations() {
        for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"] {
            let fixture = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(fixture.path()).unwrap();
            let text = format!("// ababa λ😀 {}\npub fn anchor() {{}}\n", "x".repeat(129));
            let raw = match encoding {
                "utf-8" => text.as_bytes().to_vec(),
                "utf-8-bom" => [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat(),
                "utf-16le" => std::iter::once(0xfeff)
                    .chain(text.encode_utf16())
                    .flat_map(u16::to_le_bytes)
                    .collect(),
                "utf-16be" => std::iter::once(0xfeff)
                    .chain(text.encode_utf16())
                    .flat_map(u16::to_be_bytes)
                    .collect(),
                _ => unreachable!(),
            };
            std::fs::write(root.join("module.rs"), &raw).unwrap();
            let cancellation = Cancellation::new();
            let mut service = FirstSliceService::new(2).unwrap();
            let first = service
                .index_repository_with_mode(
                    &root,
                    rootlight_service::FirstSliceIndexMode::Structural,
                    &cancellation,
                )
                .unwrap();
            let file = service
                .inspect_generation(
                    first.repository,
                    Some(first.generation),
                    &cancellation,
                    |snapshot| {
                        snapshot
                            .document()
                            .files
                            .iter()
                            .find(|file| file.path == "module.rs")
                            .unwrap()
                            .id
                    },
                )
                .unwrap();
            std::fs::write(root.join("module.rs"), "// successor\npub fn anchor() {}\n").unwrap();
            let second = service
                .index_repository_with_mode(
                    &root,
                    rootlight_service::FirstSliceIndexMode::Structural,
                    &cancellation,
                )
                .unwrap();
            let mut request = parse(json!({"mode":"source_find","query":"aba","generation":first.generation,"find":{"file":file,"read_matches":true}})).unwrap();
            let found =
                query_existing(&service, first.repository, &request, true, &cancellation).unwrap();
            assert_eq!(found["stop"], "end_of_source");
            assert!(found["continuation"].is_null());
            assert_eq!(
                found["file"]["encoding"],
                if encoding.starts_with("utf-8") {
                    "utf-8"
                } else {
                    encoding
                }
            );
            assert_eq!(found["scanned"]["end_byte"], raw.len());
            let matches = found["matches"].as_array().unwrap();
            assert_eq!(matches.len(), 2);
            let reads = found["source_reads"].as_array().unwrap();
            assert_eq!(reads.len(), matches.len());
            let (start, width) = if encoding.starts_with("utf-8") {
                (3 + usize::from(encoding == "utf-8-bom") * 3, 1)
            } else {
                (2 + 3 * 2, 2)
            };
            for (index, matched) in matches.iter().enumerate() {
                assert_eq!(matched["generation"], first.generation.to_string());
                assert_eq!(matched["span"]["start_byte"], start + index * 2 * width);
                assert_eq!(matched["span"]["end_byte"], start + (index * 2 + 3) * width);
                let bytes = reads[index]["data"]["chunks"][0]["bytes"]
                    .as_array()
                    .unwrap();
                let actual: Vec<_> = bytes
                    .iter()
                    .map(|value| u8::try_from(value.as_u64().unwrap()).unwrap())
                    .collect();
                let begin = start + index * 2 * width;
                assert_eq!(actual, raw[begin..begin + 3 * width]);
            }
            request.query = "x".repeat(129);
            assert_eq!(
                query_existing(&service, first.repository, &request, true, &cancellation).unwrap()
                    ["matches"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            request.generation = Some(second.generation);
            assert!(
                query_existing(&service, first.repository, &request, true, &cancellation).unwrap()
                    ["matches"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn literal_pages_replay_real_cursor_parts_without_claiming_an_unscanned_total() {
        let fixture = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(fixture.path()).unwrap();
        let text = format!("// {}\npub fn anchor() {{}}\n", "aba".repeat(260));
        std::fs::write(root.join("module.rs"), &text).unwrap();
        let cancellation = Cancellation::new();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository_with_mode(
                &root,
                rootlight_service::FirstSliceIndexMode::Structural,
                &cancellation,
            )
            .unwrap();
        let file = service
            .inspect_generation(
                receipt.repository,
                Some(receipt.generation),
                &cancellation,
                |snapshot| {
                    snapshot
                        .document()
                        .files
                        .iter()
                        .find(|file| file.path == "module.rs")
                        .unwrap()
                        .id
                },
            )
            .unwrap();
        let mut request = parse(json!({"mode":"source_find","query":"aba","generation":receipt.generation,"find":{"file":file}})).unwrap();
        let mut end = 0;
        let mut starts = Vec::new();
        let mut page_counts = Vec::new();
        loop {
            let found = query_existing(&service, receipt.repository, &request, true, &cancellation)
                .unwrap();
            assert!(found.get("total_matches").is_none());
            assert_eq!(found["scanned"]["start_byte"], end);
            let next_end = found["scanned"]["end_byte"].as_u64().unwrap();
            assert!(next_end > end && next_end <= u64::try_from(text.len()).unwrap());
            end = next_end;
            let matches = found["matches"].as_array().unwrap();
            page_counts.push(matches.len());
            starts.extend(
                matches
                    .iter()
                    .map(|reference| reference["span"]["start_byte"].as_u64().unwrap()),
            );
            assert!(found["source_reads"].as_array().unwrap().is_empty());
            if found["continuation"].is_null() {
                assert_eq!(found["stop"], "end_of_source");
                assert_eq!(end, u64::try_from(text.len()).unwrap());
                break;
            }
            assert_eq!(found["stop"], "match_limit");
            request.find.as_mut().unwrap().cursor =
                Some(serde_json::from_value(found["continuation"].clone()).unwrap());
        }
        assert_eq!(page_counts, [128, 128, 4]);
        assert_eq!(
            starts,
            (0..260).map(|index| 3 + index * 3).collect::<Vec<_>>()
        );
    }
}
