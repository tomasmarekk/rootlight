//! Real-process literal evidence uses a current isolated fixture and the shared transcript.
//! Physical-byte oracles are independent of parser symbols and lexical token semantics.

use rootlight_mcp_contract::source_find::{SourceFindEnvelope, SourceFindStop};
use rootlight_mcp_contract::source_text::SourceTextEncoding;

use super::*;

const TEXT: &str = "// ababa a\u{1f642}a\u{1f642}a\r\n// prefixzzneedlefragmentzzsuffix\r\npub fn literal_anchor() -> usize { 1 }\r\n";
const LITERAL: &str = "needlefragment";

/// Actual public-process observations from the isolated physical-source fixture.
#[derive(Serialize)]
pub(super) struct Evidence {
    fixture: &'static str,
    physical_files: usize,
    checked_matches: usize,
    continued_pages: usize,
    rejected_cursor_mutations: usize,
    rejected_unsupported_profiles: usize,
    lexical_substring_negative: bool,
    exact_match_reads: bool,
    source_body_bytes_returned_by_find: u64,
}

struct WrittenFile {
    path: &'static str,
    bytes: Vec<u8>,
    encoding: SourceTextEncoding,
}

fn files() -> Vec<WrittenFile> {
    let mut large = b"// ".to_vec();
    large.resize(4 * 1024 * 1024, b'x');
    large.extend_from_slice(b"\npub fn cancellation_anchor() {}\n");
    vec![
        WrittenFile {
            path: "src/utf8.rs",
            bytes: TEXT.as_bytes().to_vec(),
            encoding: SourceTextEncoding::Utf8,
        },
        WrittenFile {
            path: "src/utf16le.rs",
            bytes: encode(TEXT, SourceTextEncoding::Utf16Le),
            encoding: SourceTextEncoding::Utf16Le,
        },
        WrittenFile {
            path: "src/utf16be.rs",
            bytes: encode(TEXT, SourceTextEncoding::Utf16Be),
            encoding: SourceTextEncoding::Utf16Be,
        },
        WrittenFile {
            path: "src/cancel.rs",
            bytes: large,
            encoding: SourceTextEncoding::Utf8,
        },
    ]
}

fn encode(text: &str, encoding: SourceTextEncoding) -> Vec<u8> {
    match encoding {
        SourceTextEncoding::Utf8 => text.as_bytes().to_vec(),
        SourceTextEncoding::Utf16Le => std::iter::once(0xfeff)
            .chain(text.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect(),
        SourceTextEncoding::Utf16Be => std::iter::once(0xfeff)
            .chain(text.encode_utf16())
            .flat_map(u16::to_be_bytes)
            .collect(),
    }
}

fn expected_spans(file: &WrittenFile, literal: &str) -> Vec<(u64, u64)> {
    let encoded = encode(literal, file.encoding);
    let needle = if file.encoding == SourceTextEncoding::Utf8 {
        encoded.as_slice()
    } else {
        &encoded[2..]
    };
    file.bytes
        .windows(needle.len())
        .enumerate()
        .filter(|(start, bytes)| {
            *bytes == needle
                && (file.encoding == SourceTextEncoding::Utf8
                    || (*start >= 2 && start.is_multiple_of(2)))
        })
        .map(|(start, _)| (start as u64, (start + needle.len()) as u64))
        .collect()
}

fn prepare(root: &Path, files: &[WrittenFile]) -> Result<(), VerticalError> {
    fs::create_dir_all(root.join("src")).map_err(|source| VerticalError::Io {
        action: "create literal fixture",
        source,
    })?;
    for file in files {
        fs::write(root.join(file.path), &file.bytes).map_err(|source| VerticalError::Io {
            action: "write literal physical fixture",
            source,
        })?;
    }
    Ok(())
}

fn located_file(
    process: &mut McpProcess,
    catalog: &ToolCatalog,
    transcript: &mut TranscriptWriter,
    index: &IndexReceipt,
    file: &WrittenFile,
) -> Result<Value, VerticalError> {
    let response = call_tool(
        &format!("literal.locate.{}", file.path),
        process,
        catalog,
        transcript,
        "code.locate",
        json!({
            "repository": {"repository_id": index.repository}, "generation": index.generation,
            "query": file.path, "kinds": ["file"], "search_modes": ["path"], "max_results": 10, "response_profile": "compact"
        }),
    )?;
    require_tool_success(&response, "code.locate")?;
    let hits =
        response.structured["data"]["matches"]
            .as_array()
            .ok_or(VerticalError::Invariant(
                "literal file locator omitted its matches",
            ))?;
    let matching: Vec<_> = hits.iter().filter(|hit| hit["path"] == file.path).collect();
    if matching.len() != 1 {
        return Err(VerticalError::Invariant(
            "literal fixture file was not uniquely discoverable",
        ));
    }
    let reference = matching[0]["source_ref"].clone();
    if reference["repository"] != index.repository
        || reference["generation"] != index.generation
        || reference["span"]["file"].as_str().is_none()
        || reference["content_hash"] != rootlight_ids::content_hash(&file.bytes).to_string()
    {
        return Err(VerticalError::Invariant(
            "literal fixture file identity was not source bound",
        ));
    }
    Ok(reference)
}

fn arguments(index: &IndexReceipt, reference: &Value, literal: &str) -> Value {
    json!({"repository":{"repository_id": index.repository}, "generation": index.generation,
        "file_id": reference["span"]["file"], "literal": literal, "response_profile": "compact"})
}

fn page(
    process: &mut McpProcess,
    catalog: &ToolCatalog,
    transcript: &mut TranscriptWriter,
    label: &str,
    arguments: Value,
) -> Result<SourceFindEnvelope, VerticalError> {
    let outcome = call_tool(
        label,
        process,
        catalog,
        transcript,
        "source.find",
        arguments,
    )?;
    require_tool_success(&outcome, "source.find")?;
    require_matrix_usage(&outcome.structured)?;
    serde_json::from_value(outcome.structured).map_err(|source| VerticalError::Json {
        action: "validate literal standalone response",
        source,
    })
}

fn verify_page(
    page: &SourceFindEnvelope,
    index: &IndexReceipt,
    file: &WrittenFile,
    reference: &Value,
    literal: &str,
    start: u64,
    scan_limit: u64,
) -> Result<Vec<(u64, u64)>, VerticalError> {
    let expected = expected_spans(file, literal);
    if page.repository.repository_id.to_string() != index.repository
        || page.generation.to_string() != index.generation
        || page.data.file.id.to_string() != reference["span"]["file"].as_str().unwrap_or("")
        || page.data.file.content_hash.to_string()
            != reference["content_hash"].as_str().unwrap_or("")
        || page.data.file.path != file.path
        || page.data.file.encoding != file.encoding
        || page.data.file.language != "rust"
        || page.data.file.generated
        || page.data.file.extracted_from.is_some()
        || page.data.file.byte_length != file.bytes.len() as u64
        || page.usage.source_bytes != 0
        || page.data.scanned_start_byte != start
        || page.data.scanned_end_byte - start > scan_limit
    {
        return Err(VerticalError::Invariant(
            "literal page disagrees with its physical source oracle",
        ));
    }
    let actual: Vec<_> = page
        .data
        .matches
        .iter()
        .map(|hit| (hit.span().start_byte(), hit.span().end_byte()))
        .collect();
    let in_page: Vec<_> = expected
        .into_iter()
        .filter(|(_, end)| *end > start && *end <= page.data.scanned_end_byte)
        .collect();
    if actual != in_page {
        return Err(VerticalError::Invariant(
            "literal page omitted or invented a physical match",
        ));
    }
    Ok(actual)
}

fn cell(state: &'static str, reason: &'static str) -> ToolMatrixCell {
    ToolMatrixCell::executed(
        "source.find",
        state,
        "success",
        reason,
        "matched_exact_repository_generation_file_hash",
        "validated_physical_scan_completion",
        "measured_nonplaceholder",
        None,
    )
}

/// Drives current daemon and MCP binaries, failing on any missing source obligation.
/// Appends only observed state cells; failures leave the shared transcript intact.
///
/// # Errors
/// Returns process, transport, schema, or physical-source evidence failures.
pub(super) fn exercise(
    daemon_binary: &Path,
    mcp_binary: &Path,
    temporary: &Path,
    transcript: &mut TranscriptWriter,
    cells: &mut Vec<ToolMatrixCell>,
    storage_trace: &Path,
) -> Result<Evidence, VerticalError> {
    let root = temporary.join("literal-repository");
    let written = files();
    prepare(&root, &written)?;
    let paths = RuntimePaths::new(
        temporary.join("literal-state"),
        temporary.join("literal-runtime"),
    )
    .map_err(VerticalError::Runtime)?;
    let environment = Environment::new(&paths);
    let mut daemon = SupervisedDaemon::spawn(daemon_binary, &environment)?;
    wait_until_ready(&paths, &mut daemon)?;
    let (mut process, catalog, _) =
        open_session("literal", mcp_binary, &environment, temporary, transcript)?;
    let base = index_repository("literal.base", &mut process, &catalog, transcript, &root)?;
    let mut evidence = Evidence {
        fixture: "scalar-literal-physical-v1",
        physical_files: written.len() + 2,
        checked_matches: 0,
        continued_pages: 0,
        rejected_cursor_mutations: 0,
        rejected_unsupported_profiles: 0,
        lexical_substring_negative: false,
        exact_match_reads: false,
        source_body_bytes_returned_by_find: 0,
    };
    let references = written
        .iter()
        .map(|file| located_file(&mut process, &catalog, transcript, &base, file))
        .collect::<Result<Vec<_>, _>>()?;
    for (file, reference) in written.iter().take(3).zip(&references) {
        for literal in [LITERAL, "aba", "a\u{1f642}a"] {
            let start = if file.encoding == SourceTextEncoding::Utf8 {
                0
            } else {
                2
            };
            let result = page(
                &mut process,
                &catalog,
                transcript,
                "literal.complete",
                arguments(&base, reference, literal),
            )?;
            let matches = verify_page(
                &result,
                &base,
                file,
                reference,
                literal,
                start,
                64 * 1024 * 1024,
            )?;
            if result.data.stop != SourceFindStop::EndOfSource
                || matches != expected_spans(file, literal)
            {
                return Err(VerticalError::Invariant(
                    "literal complete page omitted an authored match",
                ));
            }
            evidence.checked_matches += matches.len();
            read_matches(
                &mut process,
                &catalog,
                transcript,
                &base,
                &result,
                file,
                literal,
            )?;
        }
        evidence.continued_pages +=
            continued(&mut process, &catalog, transcript, &base, file, reference)?;
    }
    evidence.exact_match_reads = true;
    lexical_negative(&mut process, &catalog, transcript, &base)?;
    evidence.lexical_substring_negative = true;
    cells.push(cell(
        "fresh_success",
        "literal spans and exact source reads match all three physical encodings",
    ));
    cells.push(cell(
        "truncated",
        "bounded source pages cover every original byte and overlapping match exactly once",
    ));
    (
        evidence.rejected_cursor_mutations,
        evidence.rejected_unsupported_profiles,
    ) = cursor_mutations(&mut process, &catalog, transcript, &base, &references)?;
    error_states(
        &mut process,
        &catalog,
        transcript,
        &base,
        &references[0],
        cells,
    )?;
    unsupported_version(
        &mut process,
        &catalog,
        transcript,
        &base,
        &references[0],
        cells,
    )?;
    cancel_find(
        &mut process,
        &catalog,
        transcript,
        &base,
        &references[3],
        cells,
    )?;
    let changed = TEXT.replace("usize { 1 }", "usize { 2 }");
    fs::write(root.join(written[0].path), &changed).map_err(|source| VerticalError::Io {
        action: "update literal retained-generation fixture",
        source,
    })?;
    let head = index_repository("literal.head", &mut process, &catalog, transcript, &root)?;
    if head.generation == base.generation {
        return Err(VerticalError::Invariant(
            "literal fixture failed to publish a changed generation",
        ));
    }
    let retained = page(
        &mut process,
        &catalog,
        transcript,
        "literal.retained",
        arguments(&base, &references[0], LITERAL),
    )?;
    verify_page(
        &retained,
        &base,
        &written[0],
        &references[0],
        LITERAL,
        0,
        64 * 1024 * 1024,
    )?;
    cells.push(cell(
        "stale_retained_generation",
        "exact old generation still supplies its original immutable literal spans",
    ));
    stop_mcp(&mut process, "literal MCP stderr")?;
    assert_private_source_absent("literal daemon stderr", &daemon.shutdown()?)?;
    wait_until_absent(&paths)?;
    let mut daemon = SupervisedDaemon::spawn(daemon_binary, &environment)?;
    wait_until_ready(&paths, &mut daemon)?;
    let (mut process, catalog, _) = open_session(
        "literal-restart",
        mcp_binary,
        &environment,
        temporary,
        transcript,
    )?;
    let restored = page(
        &mut process,
        &catalog,
        transcript,
        "literal.restart",
        arguments(&base, &references[0], LITERAL),
    )?;
    if restored.data != retained.data {
        return Err(VerticalError::Invariant(
            "literal restart changed retained source evidence",
        ));
    }
    cells.push(cell(
        "restart_recovered",
        "durable restart preserves literal source identity and exact retained matches",
    ));
    stop_mcp(&mut process, "literal restart MCP stderr")?;
    assert_private_source_absent("literal restart daemon stderr", &daemon.shutdown()?)?;
    wait_until_absent(&paths)?;
    storage_failures(
        daemon_binary,
        mcp_binary,
        temporary,
        transcript,
        &written[0],
        &WrittenFile {
            path: written[0].path,
            bytes: changed.into_bytes(),
            encoding: written[0].encoding,
        },
        cells,
        storage_trace,
    )?;
    Ok(evidence)
}

fn read_matches(
    process: &mut McpProcess,
    catalog: &ToolCatalog,
    transcript: &mut TranscriptWriter,
    index: &IndexReceipt,
    page: &SourceFindEnvelope,
    file: &WrittenFile,
    literal: &str,
) -> Result<(), VerticalError> {
    let references: Vec<_> = page
        .data
        .matches
        .iter()
        .map(|source| json!({"source_ref": source}))
        .collect();
    let outcome = call_tool(
        "literal.read-matches",
        process,
        catalog,
        transcript,
        "source.read",
        json!({
            "repository":{"repository_id":index.repository}, "generation":index.generation,
            "references":references, "include_line_numbers":false, "merge_overlaps":false, "encoding":"lossless_text", "response_profile":"compact"
        }),
    )?;
    require_tool_success(&outcome, "source.read")?;
    assert_read_correlation(&outcome.structured, &index.repository, &index.generation)?;
    let chunks =
        outcome.structured["data"]["chunks"]
            .as_array()
            .ok_or(VerticalError::Invariant(
                "literal references were not exactly readable",
            ))?;
    if chunks.len() != page.data.matches.len()
        || chunks
            .iter()
            .zip(&page.data.matches)
            .any(|(chunk, reference)| {
                chunk["content"] != literal
                    || chunk["source_ref"] != json!(reference)
                    || chunk["path"] != file.path
                    || chunk["physical_encoding"] != json!(file.encoding)
            })
    {
        return Err(VerticalError::Invariant(
            "literal source-read content disagrees with the authored query",
        ));
    }
    Ok(())
}

fn continued(
    process: &mut McpProcess,
    catalog: &ToolCatalog,
    transcript: &mut TranscriptWriter,
    index: &IndexReceipt,
    file: &WrittenFile,
    reference: &Value,
) -> Result<usize, VerticalError> {
    let literal = if file.encoding == SourceTextEncoding::Utf8 {
        "aba"
    } else {
        "a🙂a"
    };
    let mut input = arguments(index, reference, literal);
    input["maximum_scan_bytes"] = json!(4);
    input["maximum_matches"] = json!(1);
    let mut start = if file.encoding == SourceTextEncoding::Utf8 {
        0
    } else {
        2
    };
    let mut actual = Vec::new();
    let mut cursors = BTreeSet::new();
    let mut pages = 0;
    loop {
        let result = page(
            process,
            catalog,
            transcript,
            "literal.continued",
            input.clone(),
        )?;
        actual.extend(verify_page(
            &result, index, file, reference, literal, start, 4,
        )?);
        pages += 1;
        if result.data.stop == SourceFindStop::EndOfSource {
            break;
        }
        if result.data.scanned_end_byte <= start || pages > file.bytes.len() {
            return Err(VerticalError::Invariant(
                "literal continuation failed to progress",
            ));
        }
        start = result.data.scanned_end_byte;
        let cursor = result.next_cursor.0.ok_or(VerticalError::Invariant(
            "literal partial page omitted its cursor",
        ))?;
        if !cursors.insert(cursor.as_str().to_owned()) {
            return Err(VerticalError::Invariant(
                "literal continuation repeated a cursor",
            ));
        }
        input["cursor"] = json!(cursor);
    }
    if actual != expected_spans(file, literal) || pages < 2 {
        return Err(VerticalError::Invariant(
            "literal continuation lost an overlap or crossed-page match",
        ));
    }
    Ok(pages)
}

fn lexical_negative(
    process: &mut McpProcess,
    catalog: &ToolCatalog,
    transcript: &mut TranscriptWriter,
    index: &IndexReceipt,
) -> Result<(), VerticalError> {
    let outcome = call_tool(
        "literal.lexical-negative",
        process,
        catalog,
        transcript,
        "code.locate",
        json!({
            "repository":{"repository_id":index.repository},"generation":index.generation,"query":LITERAL,
            "search_modes":["lexical"],"max_results":10,"response_profile":"compact"
        }),
    )?;
    require_tool_success(&outcome, "code.locate")?;
    if outcome.structured["data"]["matches"]
        .as_array()
        .is_none_or(|hits| !hits.is_empty())
    {
        return Err(VerticalError::Invariant(
            "literal-only substring unexpectedly became a lexical code token",
        ));
    }
    Ok(())
}

fn cursor_mutations(
    process: &mut McpProcess,
    catalog: &ToolCatalog,
    transcript: &mut TranscriptWriter,
    index: &IndexReceipt,
    references: &[Value],
) -> Result<(usize, usize), VerticalError> {
    let mut original = arguments(index, &references[0], "aba");
    original["maximum_scan_bytes"] = json!(4);
    let result = page(
        process,
        catalog,
        transcript,
        "literal.cursor-seed",
        original.clone(),
    )?;
    let cursor = result.next_cursor.0.ok_or(VerticalError::Invariant(
        "literal cursor fixture was not partial",
    ))?;
    original["cursor"] = json!(cursor);
    let mutations = [
        ("literal", json!("abb")),
        ("maximum_scan_bytes", json!(5)),
        ("maximum_matches", json!(1)),
        ("file_id", references[1]["span"]["file"].clone()),
        (
            "generation",
            json!(GenerationId::from_bytes([0xdb; 20]).to_string()),
        ),
        (
            "repository",
            json!({"repository_id":RepositoryId::from_bytes([0xda;16]).to_string()}),
        ),
    ];
    for (field, value) in &mutations {
        let mut altered = original.clone();
        altered[*field] = value.clone();
        let outcome = call_tool(
            "literal.cursor-mutation",
            process,
            catalog,
            transcript,
            "source.find",
            altered,
        )?;
        if !outcome.is_error || outcome.structured["error"]["code"] != "INVALID_CURSOR" {
            return Err(VerticalError::Invariant(
                "literal cursor admitted a changed request binding",
            ));
        }
    }
    let mut unsupported_profile = original.clone();
    unsupported_profile["response_profile"] = json!("standard");
    if catalog
        .validate_input("source.find", &unsupported_profile)
        .is_ok()
    {
        return Err(VerticalError::Invariant(
            "literal input schema advertised an unsupported representation",
        ));
    }
    let rejected = call_tool_unchecked(
        "literal.unsupported-profile",
        process,
        catalog,
        transcript,
        "source.find",
        unsupported_profile,
    )?;
    require_domain_error(&rejected, "INVALID_ARGUMENT")?;
    let mut forged = cursor.as_str().as_bytes().to_vec();
    let middle = forged.len() / 2;
    let byte = forged
        .get_mut(middle)
        .ok_or(VerticalError::Invariant("literal cursor was empty"))?;
    *byte = if *byte == b'a' { b'b' } else { b'a' };
    original["cursor"] = json!(
        String::from_utf8(forged)
            .map_err(|_| VerticalError::Invariant("literal cursor stopped being ASCII"))?
    );
    let outcome = call_tool(
        "literal.cursor-forgery",
        process,
        catalog,
        transcript,
        "source.find",
        original,
    )?;
    if !outcome.is_error || outcome.structured["error"]["code"] != "INVALID_CURSOR" {
        return Err(VerticalError::Invariant(
            "literal cursor admitted altered authentication bytes",
        ));
    }
    Ok((mutations.len() + 1, 1))
}

fn error_states(
    process: &mut McpProcess,
    catalog: &ToolCatalog,
    transcript: &mut TranscriptWriter,
    index: &IndexReceipt,
    reference: &Value,
    cells: &mut Vec<ToolMatrixCell>,
) -> Result<(), VerticalError> {
    for (state, field, value, code) in [
        (
            "missing_repository",
            "repository",
            json!({"repository_id":RepositoryId::from_bytes([0xec;16]).to_string()}),
            "NOT_FOUND",
        ),
        (
            "missing_generation",
            "generation",
            json!(GenerationId::from_bytes([0xed; 20]).to_string()),
            "STALE_GENERATION",
        ),
    ] {
        let mut input = arguments(index, reference, LITERAL);
        input[field] = value;
        let outcome = call_tool(
            &format!("literal.{state}"),
            process,
            catalog,
            transcript,
            "source.find",
            input,
        )?;
        record_matrix_error(
            cells,
            "source.find",
            state,
            outcome,
            code,
            "missing immutable source authority produces an explicit source-free error",
        )?;
    }
    Ok(())
}

fn unsupported_version(
    process: &mut McpProcess,
    catalog: &ToolCatalog,
    transcript: &mut TranscriptWriter,
    index: &IndexReceipt,
    reference: &Value,
    cells: &mut Vec<ToolMatrixCell>,
) -> Result<(), VerticalError> {
    let input = arguments(index, reference, LITERAL);
    catalog.validate_input("source.find", &input)?;
    let response = process.request(
        transcript,
        "literal.unsupported-version".to_owned(),
        "tools/call",
        json!({
            "name": "source.find",
            "arguments": input,
            "_meta": {"rootlight/toolContractVersion": "99.0"}
        }),
    )?;
    let outcome = catalog.validate_result("source.find", &response.response)?;
    record_matrix_error(
        cells,
        "source.find",
        "unsupported_capability",
        outcome,
        "PROTOCOL_MISMATCH",
        "unsupported literal-tool protocol major is rejected before input or source access",
    )
}

fn cancel_find(
    process: &mut McpProcess,
    catalog: &ToolCatalog,
    transcript: &mut TranscriptWriter,
    index: &IndexReceipt,
    reference: &Value,
    cells: &mut Vec<ToolMatrixCell>,
) -> Result<(), VerticalError> {
    let literal = format!("{}z", "x".repeat(60 * 1024 - 1));
    let input = arguments(index, reference, &literal);
    catalog.validate_input("source.find", &input)?;
    let id = process.begin_tool_request(
        transcript,
        "literal.cancellable-request",
        "source.find",
        catalog.contract_version("source.find")?,
        input,
    )?;
    process.notification(
        transcript,
        "literal.cancel".to_owned(),
        "notifications/cancelled",
        json!({"requestId":id,"reason":"cancel literal scan"}),
    )?;
    let ping = process.request(
        transcript,
        "literal.cancel-follow-up".to_owned(),
        "ping",
        json!({}),
    )?;
    if ping.response["result"] != json!({}) {
        return Err(VerticalError::Invariant(
            "literal cancellation did not preserve the transport",
        ));
    }
    let mut cell = ToolMatrixCell::cancelled(
        "source.find",
        "cancelled public literal request emits no partial response and leaves the transport reusable",
    );
    cell.identity = "cancelled_exact_generation_request";
    cell.completeness = "no_partial_literal_response";
    cells.push(cell);
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the fault fixture retains process, source, and persisted evidence bindings"
)]
fn storage_failures(
    daemon_binary: &Path,
    mcp_binary: &Path,
    temporary: &Path,
    transcript: &mut TranscriptWriter,
    original: &WrittenFile,
    changed: &WrittenFile,
    cells: &mut Vec<ToolMatrixCell>,
    storage_trace: &Path,
) -> Result<(), VerticalError> {
    for (case, state) in ["corrupt_persisted_state", "incompatible_persisted_schema"]
        .into_iter()
        .enumerate()
    {
        let target = temporary.join(format!("literal-{state}"));
        let root = target.join("repository");
        prepare(&root, std::slice::from_ref(original))?;
        // Keep the socket beneath the same short root as the main fixture so
        // isolated fault cases also fit macOS's Unix socket path limit.
        let paths = RuntimePaths::new(target.join("state"), temporary.join(format!("s{case}")))
            .map_err(VerticalError::Runtime)?;
        let environment = Environment::new(&paths);
        // The daemon must create each private storage tree. A regular recursive
        // copy does not preserve the protected directory ACL contract on Windows.
        let mut daemon = SupervisedDaemon::spawn(daemon_binary, &environment)?;
        wait_until_ready(&paths, &mut daemon)?;
        let (mut process, catalog, _) = open_session(
            "literal-storage-baseline",
            mcp_binary,
            &environment,
            temporary,
            transcript,
        )?;
        // Keep one deep provider frame across restart. A structural head may
        // gain a semantic child and evict the predecessor under retention two.
        let base = index_repository_deep(
            &format!("literal.{state}.base"),
            &mut process,
            &catalog,
            transcript,
            &root,
        )?;
        let base_reference = located_file(&mut process, &catalog, transcript, &base, original)?;
        fs::write(root.join(changed.path), &changed.bytes).map_err(|source| VerticalError::Io {
            action: "update isolated storage-fault fixture",
            source,
        })?;
        let index = index_repository_deep(
            &format!("literal.{state}.head"),
            &mut process,
            &catalog,
            transcript,
            &root,
        )?;
        if base.generation == index.generation
            || index.parent_generation.as_deref() != Some(base.generation.as_str())
        {
            return Err(VerticalError::Invariant(
                "storage-fault fixture did not retain a predecessor",
            ));
        }
        let reference = located_file(&mut process, &catalog, transcript, &index, changed)?;
        let baseline = page(
            &mut process,
            &catalog,
            transcript,
            &format!("literal.{state}.baseline"),
            arguments(&index, &reference, LITERAL),
        )?;
        verify_page(
            &baseline,
            &index,
            changed,
            &reference,
            LITERAL,
            0,
            64 * 1024 * 1024,
        )?;
        if baseline.data.stop != SourceFindStop::EndOfSource {
            return Err(VerticalError::Invariant(
                "storage-fault baseline did not complete its source scan",
            ));
        }
        // Exercise a valid same-generation activation before the fault as well
        // as the startup watcher's possible republication after recovery.
        if state == "corrupt_persisted_state" {
            let reactivation = index_repository_deep(
                &format!("literal.{state}.baseline-reactivation"),
                &mut process,
                &catalog,
                transcript,
                &root,
            )?;
            verify_storage_republication_receipt(&index, &reactivation)?;
        }
        stop_mcp(&mut process, "literal storage baseline MCP stderr")?;
        assert_private_source_absent(
            "literal storage baseline daemon stderr",
            &daemon.shutdown()?,
        )?;
        wait_until_absent(&paths)?;
        let manifest = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(&index.repository)
            .join(&index.generation)
            .join("manifest.json");
        let before = record_storage_trace(
            storage_trace,
            state,
            "before_mutation",
            &index,
            &manifest,
            None,
        )?;
        let original_manifest = fs::read(&manifest).map_err(|source| VerticalError::Io {
            action: "read isolated literal generation manifest",
            source,
        })?;
        let replacement = if state == "corrupt_persisted_state" {
            b"{".to_vec()
        } else {
            let mut value: Value =
                serde_json::from_slice(&original_manifest).map_err(|source| {
                    VerticalError::Json {
                        action: "decode literal generation manifest",
                        source,
                    }
                })?;
            value["version"] = json!(u16::MAX);
            serde_json::to_vec(&value).map_err(|source| VerticalError::Json {
                action: "encode incompatible literal manifest",
                source,
            })?
        };
        fs::write(&manifest, &replacement).map_err(|source| VerticalError::Io {
            action: "write isolated literal storage fault",
            source,
        })?;
        if fs::read(&manifest).map_err(|source| VerticalError::Io {
            action: "verify exact persisted fault bytes",
            source,
        })? != replacement
        {
            return Err(VerticalError::Invariant(
                "persisted fault mutation did not conserve its bytes",
            ));
        }
        let altered = record_storage_trace(
            storage_trace,
            state,
            "after_mutation",
            &index,
            &manifest,
            None,
        )?;
        let mut daemon = SupervisedDaemon::spawn(daemon_binary, &environment)?;
        // Reach the daemon during deferred validation. The pinned request may
        // observe rejection or an already authenticated republication.
        wait_until_requestable(&paths, &mut daemon)?;
        let (mut process, catalog, _) = open_session(
            "literal-storage-fault",
            mcp_binary,
            &environment,
            temporary,
            transcript,
        )?;
        let outcome = call_tool_with_admission_retry(
            &format!("literal.{state}"),
            &mut process,
            &catalog,
            transcript,
            "source.find",
            arguments(&index, &reference, LITERAL),
        )?;
        record_storage_trace(
            storage_trace,
            state,
            "after_source_find",
            &index,
            &manifest,
            Some(&outcome.structured),
        )?;
        // Publication may precede terminal journal state; status must observe both.
        wait_until_journal_idle(&paths.operation_journal_path())?;
        let status = call_tool(
            &format!("literal.{state}.recovery-status"),
            &mut process,
            &catalog,
            transcript,
            "repo.status",
            json!({"repository":{"repository_id":index.repository},"response_profile":"compact"}),
        )?;
        record_storage_trace(
            storage_trace,
            state,
            "after_repository_status",
            &index,
            &manifest,
            Some(&status.structured),
        )?;
        let predecessor = page(
            &mut process,
            &catalog,
            transcript,
            &format!("literal.{state}.verified-predecessor"),
            arguments(&base, &base_reference, LITERAL),
        )?;
        verify_page(
            &predecessor,
            &base,
            original,
            &base_reference,
            LITERAL,
            0,
            64 * 1024 * 1024,
        )?;
        if predecessor.data.stop != SourceFindStop::EndOfSource {
            return Err(VerticalError::Invariant(
                "recovered predecessor scan did not complete",
            ));
        }
        // Force the same valid early publication that the startup watcher may
        // complete before a client can observe the restored predecessor.
        if state == "corrupt_persisted_state" {
            let early_publication = index_repository_deep(
                &format!("literal.{state}.early-republication"),
                &mut process,
                &catalog,
                transcript,
                &root,
            )?;
            verify_storage_republication_receipt(&index, &early_publication)?;
            wait_until_journal_idle(&paths.operation_journal_path())?;
        }
        let status = call_tool(
            &format!("literal.{state}.verified-predecessor-status"),
            &mut process,
            &catalog,
            transcript,
            "repo.status",
            json!({"repository":{"repository_id":index.repository},"response_profile":"compact"}),
        )?;
        require_tool_success(&status, "repo.status")?;
        let observed = record_storage_trace(
            storage_trace,
            state,
            "after_verified_predecessor_status",
            &index,
            &manifest,
            Some(&status.structured),
        )?;
        verify_storage_recovery_observation(&before, &altered, &observed, &index)?;
        // A watcher may already have republished the authored head. The attached
        // operation must still confirm that exact generation and its new custody.
        let republication = index_repository_deep(
            &format!("literal.{state}.republication"),
            &mut process,
            &catalog,
            transcript,
            &root,
        )?;
        verify_storage_republication_receipt(&index, &republication)?;
        wait_until_journal_idle(&paths.operation_journal_path())?;
        let status = call_tool(
            &format!("literal.{state}.republication-status"),
            &mut process,
            &catalog,
            transcript,
            "repo.status",
            json!({"repository":{"repository_id":index.repository},"response_profile":"compact"}),
        )?;
        let recovered = record_storage_trace(
            storage_trace,
            state,
            "after_explicit_republication",
            &index,
            &manifest,
            Some(&status.structured),
        )?;
        verify_storage_recovery(&before, &altered, &recovered, &index)?;
        // The triggering request can observe quarantine. Retry only after the
        // explicit index operation and exact manifest/activation checks prove
        // republication of the same authenticated generation.
        let outcome = if outcome.is_error {
            require_domain_error(&outcome, "STALE_GENERATION")?;
            if outcome.structured["error"]["repository"] != index.repository
                || outcome.structured["error"]["generation"] != index.generation
                || outcome.structured["error"]["retryable"] != false
            {
                return Err(VerticalError::Invariant(
                    "storage recovery refusal changed its pinned identity",
                ));
            }
            call_tool(
                &format!("literal.{state}.verified-republication"),
                &mut process,
                &catalog,
                transcript,
                "source.find",
                arguments(&index, &reference, LITERAL),
            )?
        } else {
            outcome
        };
        require_tool_success(&outcome, "source.find")?;
        require_matrix_usage(&outcome.structured)?;
        let restored: SourceFindEnvelope =
            serde_json::from_value(outcome.structured).map_err(|source| VerticalError::Json {
                action: "validate recovered literal response",
                source,
            })?;
        verify_page(
            &restored,
            &index,
            changed,
            &reference,
            LITERAL,
            0,
            64 * 1024 * 1024,
        )?;
        if restored.data != baseline.data {
            return Err(VerticalError::Invariant(
                "recovered head changed its exact source evidence",
            ));
        }
        cells.push(cell(state,
            "rejected head is quarantined; verified predecessor and a new publication restore exact authored source"));
        stop_mcp(&mut process, "literal storage MCP stderr")?;
        assert_private_source_absent("literal storage daemon stderr", &daemon.shutdown()?)?;
        wait_until_absent(&paths)?;
    }
    Ok(())
}

fn record_storage_trace(
    path: &Path,
    state: &str,
    stage: &str,
    index: &IndexReceipt,
    manifest: &Path,
    response: Option<&Value>,
) -> Result<Value, VerticalError> {
    let bytes = match fs::read(manifest) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(VerticalError::Io {
                action: "read storage fault evidence",
                source,
            });
        }
    };
    let parsed = bytes
        .as_ref()
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok());
    let repository = manifest.ancestors().nth(2).ok_or(VerticalError::Invariant(
        "storage trace lacks its repository directory",
    ))?;
    let catalog = manifest.ancestors().nth(4).ok_or(VerticalError::Invariant(
        "storage trace lacks its catalog directory",
    ))?;
    let record = json!({
        "state":state,"stage":stage,"repository":index.repository,
        "generation":index.generation,"original_operation":index.operation,
        "manifest_present":bytes.is_some(),"manifest_bytes":bytes.as_ref().map(Vec::len),
        "manifest_hash":bytes.as_ref().map(|bytes|rootlight_ids::content_hash(bytes)),
        "manifest_version":parsed.as_ref().map(|value|&value["version"]),
        "manifest_generation":parsed.as_ref().map(|value|&value["receipt"]["generation"]),
        "expected_parent":index.parent_generation,
        "repository_manifests":storage_manifests(repository)?,
        "quarantine_manifests":storage_manifests(&catalog.join("quarantine"))?,
        "response":response
    });
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| VerticalError::Io {
            action: "open storage fault evidence",
            source,
        })?;
    serde_json::to_writer(&mut file, &record).map_err(|source| VerticalError::Json {
        action: "serialize storage fault evidence",
        source,
    })?;
    file.write_all(b"\n").map_err(|source| VerticalError::Io {
        action: "finish storage fault evidence",
        source,
    })?;
    Ok(record)
}

fn storage_manifests(directory: &Path) -> Result<Vec<Value>, VerticalError> {
    let entries = fs::read_dir(directory).map_err(|source| VerticalError::Io {
        action: "enumerate isolated storage evidence",
        source,
    })?;
    let mut manifests = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| VerticalError::Io {
            action: "read isolated storage evidence entry",
            source,
        })?;
        if !entry
            .file_type()
            .map_err(|source| VerticalError::Io {
                action: "inspect isolated storage evidence entry",
                source,
            })?
            .is_dir()
        {
            continue;
        }
        let directory_name = entry.file_name();
        let directory_name = directory_name.to_str().ok_or(VerticalError::Invariant(
            "isolated catalog directory name is not UTF-8",
        ))?;
        for name in ["manifest.json", "activation.json", "logical-snapshot.json"] {
            let bytes = match fs::read(entry.path().join(name)) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(VerticalError::Io {
                        action: "read isolated storage manifest evidence",
                        source,
                    });
                }
            };
            // Exact bytes distinguish a quarantined rejected payload from a new
            // valid manifest published under the same deterministic generation.
            manifests.push(json!({
                "directory":directory_name, "name":name,
                "hash":rootlight_ids::content_hash(&bytes),
                "value":serde_json::from_slice::<Value>(&bytes).ok(),
                "bytes":bytes,
            }));
        }
    }
    manifests.sort_by_key(|value| (value["directory"].to_string(), value["name"].to_string()));
    Ok(manifests)
}

fn storage_record<'a>(
    snapshot: &'a Value,
    section: &str,
    directory: &str,
    name: &str,
) -> Result<&'a Value, VerticalError> {
    let records = snapshot[section]
        .as_array()
        .ok_or(VerticalError::Invariant(
            "storage trace omitted its manifest inventory",
        ))?;
    let mut matching = records
        .iter()
        .filter(|record| record["directory"] == directory && record["name"] == name);
    let record = matching.next().ok_or(VerticalError::Invariant(
        "storage trace omitted a required persisted record",
    ))?;
    if matching.next().is_some() || record["bytes"].as_array().is_none_or(Vec::is_empty) {
        return Err(VerticalError::Invariant(
            "storage trace record is empty or duplicated",
        ));
    }
    Ok(record)
}

fn head_activation<'a>(
    snapshot: &'a Value,
    generation: &str,
    operation: Option<&str>,
) -> Result<&'a Value, VerticalError> {
    let records = snapshot["repository_manifests"]
        .as_array()
        .ok_or(VerticalError::Invariant(
            "storage trace omitted activation inventory",
        ))?;
    let mut sequences = BTreeSet::new();
    let mut directories = BTreeSet::new();
    let mut head = None;
    let mut selected_operation = None;
    for record in records
        .iter()
        .filter(|record| record["name"] == "activation.json")
    {
        let sequence = record["value"]["global_activation_sequence"]
            .as_u64()
            .filter(|sequence| *sequence > 0)
            .ok_or(VerticalError::Invariant(
                "storage trace activation omitted its publication sequence",
            ))?;
        let directory = record["directory"]
            .as_str()
            .filter(|value| !value.is_empty());
        let recorded_generation = record["value"]["generation"]
            .as_str()
            .filter(|value| !value.is_empty());
        let recorded_operation = record["value"]["operation"]["operation"]
            .as_str()
            .filter(|value| !value.is_empty());
        if directory.is_none()
            || recorded_generation.is_none()
            || recorded_operation.is_none()
            || !sequences.insert(sequence)
            || !directories.insert(directory)
        {
            return Err(VerticalError::Invariant(
                "storage trace has invalid or ambiguous head activations",
            ));
        }
        if head.is_none_or(|(previous, _)| sequence > previous) {
            head = Some((sequence, record));
        }
        if recorded_generation == Some(generation)
            && operation.is_some()
            && recorded_operation == operation
            && selected_operation.replace(record).is_some()
        {
            return Err(VerticalError::Invariant(
                "storage trace repeats the requested activation operation",
            ));
        }
    }
    // A no-op can reactivate the same generation under a new operation. Only the
    // highest publication sequence is the current head, not every historical ID.
    let (_, record) = head.ok_or(VerticalError::Invariant(
        "storage trace omitted the head activation",
    ))?;
    if record["value"]["generation"] != generation
        || operation.is_some_and(|expected| {
            record["value"]["operation"]["operation"] != expected
                || selected_operation != Some(record)
        })
    {
        return Err(VerticalError::Invariant(
            "storage trace head activation changed its requested identity",
        ));
    }
    Ok(record)
}

fn verify_storage_recovery_observation(
    before: &Value,
    altered: &Value,
    observed: &Value,
    index: &IndexReceipt,
) -> Result<(), VerticalError> {
    let status = &observed["response"];
    let data = &status["data"];
    if status["repository"]["repository_id"] != index.repository
        || data["publication_state"] != "published"
        || data["repository_state"] != "ready"
    {
        return Err(VerticalError::Invariant(
            "storage recovery status did not retain its repository and publication",
        ));
    }
    let predecessor = index
        .parent_generation
        .as_deref()
        .ok_or(VerticalError::Invariant(
            "storage recovery fixture lacks its verified predecessor",
        ))?;
    let active = &data["active_generation"];
    if data["resolved_generation"] == predecessor && active["generation_id"] == predecessor {
        return Ok(());
    }
    // The dirty startup watcher can replace the transient predecessor before
    // status is sampled. An unchanged ID alone cannot prove that replacement.
    if data["resolved_generation"] == index.generation
        && active["generation_id"] == index.generation
    {
        return verify_storage_recovery(before, altered, observed, index);
    }
    Err(VerticalError::Invariant(
        "storage recovery status did not select its predecessor or authenticated republication",
    ))
}

fn verify_storage_republication_receipt(
    original: &IndexReceipt,
    republication: &IndexReceipt,
) -> Result<(), VerticalError> {
    if republication.repository != original.repository
        || republication.generation != original.generation
        || republication.parent_generation != original.parent_generation
        || republication.operation.is_empty()
        || republication.operation == original.operation
    {
        return Err(VerticalError::Invariant(
            "storage republication changed its inputs or reused its original operation",
        ));
    }
    Ok(())
}

fn verify_storage_recovery(
    before: &Value,
    altered: &Value,
    recovered: &Value,
    index: &IndexReceipt,
) -> Result<(), VerticalError> {
    let parent = index
        .parent_generation
        .as_deref()
        .ok_or(VerticalError::Invariant(
            "storage recovery fixture lacks its verified predecessor",
        ))?;
    for snapshot in [before, altered, recovered] {
        if snapshot["repository"] != index.repository
            || snapshot["generation"] != index.generation
            || snapshot["expected_parent"] != parent
            || snapshot["original_operation"] != index.operation
        {
            return Err(VerticalError::Invariant(
                "storage trace changed its requested identity",
            ));
        }
    }
    let original = storage_record(
        before,
        "repository_manifests",
        &index.generation,
        "manifest.json",
    )?;
    let invalid = storage_record(
        altered,
        "repository_manifests",
        &index.generation,
        "manifest.json",
    )?;
    let replacement = storage_record(
        recovered,
        "repository_manifests",
        &index.generation,
        "manifest.json",
    )?;
    let version = original["value"]["version"]
        .as_u64()
        .ok_or(VerticalError::Invariant(
            "original head manifest omitted its valid schema version",
        ))?;
    if version == u64::from(u16::MAX)
        || original["bytes"] == invalid["bytes"]
        || replacement["bytes"] == invalid["bytes"]
        || replacement["value"]["version"] != version
        || replacement["value"]["receipt"]["repository"] != index.repository
        || replacement["value"]["receipt"]["generation"] != index.generation
        || replacement["value"]["receipt"]["parent"] != parent
    {
        return Err(VerticalError::Invariant(
            "recovery did not replace the invalid head with a valid publication",
        ));
    }
    match before["state"].as_str() {
        Some("corrupt_persisted_state")
            if invalid["bytes"] == json!([b'{']) && invalid["value"].is_null() => {}
        Some("incompatible_persisted_schema") if invalid["value"]["version"] == u16::MAX => {
            let mut restored_version = invalid["value"].clone();
            restored_version["version"] = json!(version);
            if restored_version != original["value"] {
                return Err(VerticalError::Invariant(
                    "schema fault changed more than its version",
                ));
            }
        }
        _ => {
            return Err(VerticalError::Invariant(
                "storage trace did not prove its authored fault",
            ));
        }
    }
    for snapshot in [altered, recovered] {
        if storage_record(before, "repository_manifests", parent, "manifest.json")?
            != storage_record(snapshot, "repository_manifests", parent, "manifest.json")?
        {
            return Err(VerticalError::Invariant(
                "recovery changed its verified predecessor manifest",
            ));
        }
    }
    let quarantined =
        recovered["quarantine_manifests"]
            .as_array()
            .ok_or(VerticalError::Invariant(
                "storage trace omitted quarantine inventory",
            ))?;
    let quarantine_binding = format!("-{}-{}-", index.repository, index.generation);
    if !before["quarantine_manifests"]
        .as_array()
        .is_some_and(Vec::is_empty)
        || quarantined
            .iter()
            .filter(|record| {
                record["name"] == "manifest.json"
                    && record["bytes"] == invalid["bytes"]
                    && record["hash"] == invalid["hash"]
                    && record["directory"].as_str().is_some_and(|name| {
                        name.starts_with("generation-") && name.contains(&quarantine_binding)
                    })
            })
            .count()
            != 1
    {
        return Err(VerticalError::Invariant(
            "recovery did not retain the exact rejected head in quarantine",
        ));
    }
    // The same public generation ID can legitimately recur. Its full sidecar
    // binds the reconstructed manifest, configuration, provider set and format.
    let logical = storage_record(
        before,
        "repository_manifests",
        &index.generation,
        "logical-snapshot.json",
    )?;
    if logical
        != storage_record(
            recovered,
            "repository_manifests",
            &index.generation,
            "logical-snapshot.json",
        )?
    {
        return Err(VerticalError::Invariant(
            "recovery changed the head's complete logical input identity",
        ));
    }
    let payload = logical["value"]["payload"]
        .as_str()
        .ok_or(VerticalError::Invariant(
            "logical sidecar omitted its authenticated payload",
        ))?;
    let identity: Value = serde_json::from_str(payload).map_err(|source| VerticalError::Json {
        action: "decode recovered logical identity",
        source,
    })?;
    if logical["value"]["digest"] != json!(rootlight_ids::content_hash(payload.as_bytes()))
        || identity["repository"] != index.repository
        || identity["generation"] != index.generation
        || identity["parent"] != parent
        || [
            "manifest_hash",
            "configuration_hash",
            "provider_set_hash",
            "schema_version",
            "hash",
        ]
        .iter()
        .any(|key| identity[*key].as_str().is_none_or(str::is_empty))
    {
        return Err(VerticalError::Invariant(
            "logical sidecar did not authenticate the expected recovery inputs",
        ));
    }
    let previous = head_activation(before, &index.generation, None)?;
    let activations = |snapshot: &Value| -> Vec<Value> {
        snapshot["repository_manifests"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|record| record["name"] == "activation.json")
            .cloned()
            .collect()
    };
    let baseline_activations = activations(before);
    let mut requested = baseline_activations
        .iter()
        .filter(|record| record["value"]["operation"]["operation"] == index.operation);
    if requested
        .next()
        .is_none_or(|record| record["value"]["generation"] != index.generation)
        || requested.next().is_some()
        || baseline_activations != activations(altered)
    {
        return Err(VerticalError::Invariant(
            "storage fault did not conserve its requested publication and activation baseline",
        ));
    }
    let published = head_activation(recovered, &index.generation, None)?;
    let old_sequence = previous["value"]["global_activation_sequence"]
        .as_u64()
        .ok_or(VerticalError::Invariant(
            "old activation omitted its publication sequence",
        ))?;
    // A watcher can reactivate the requested generation before the daemon stops.
    // Recovery must advance beyond every operation in that frozen fault baseline.
    if baseline_activations.iter().any(|record| {
        record["value"]["operation"]["operation"] == published["value"]["operation"]["operation"]
            || record["directory"] == published["directory"]
    }) || published["value"]["global_activation_sequence"]
        .as_u64()
        .is_none_or(|sequence| sequence <= old_sequence)
    {
        return Err(VerticalError::Invariant(
            "recovery reused the rejected head's activation operation",
        ));
    }
    let status = &recovered["response"]["data"];
    if status["publication_state"] != "published"
        || status["repository_state"] != "ready"
        || status["resolved_generation"] != index.generation
        || status["active_generation"]["generation_id"] != index.generation
        || status["active_generation"]["parent_generation"] != parent
        || status["logical_snapshot"]["hash"] != identity["hash"]
    {
        return Err(VerticalError::Invariant(
            "repository status did not confirm the exact recovered publication",
        ));
    }
    Ok(())
}

fn stop_mcp(process: &mut McpProcess, surface: &'static str) -> Result<(), VerticalError> {
    let stderr = process.shutdown()?;
    assert_private_source_absent(surface, &stderr)?;
    // Reader completion closes the race where a cancelled response arrives after
    // the final matching request but before process EOF.
    verify_closed_output(&process.output)
}

fn verify_closed_output(output: &Receiver<OutputEvent>) -> Result<(), VerticalError> {
    let mut eof = false;
    for event in output.try_iter() {
        match event {
            OutputEvent::Eof if !eof => eof = true,
            _ => {
                return Err(VerticalError::Invariant(
                    "literal MCP emitted unconsumed output after its final request",
                ));
            }
        }
    }
    if !eof {
        return Err(VerticalError::Invariant(
            "literal MCP shutdown did not observe stdout EOF",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{WrittenFile, encode, expected_spans, files};
    use rootlight_mcp_contract::source_text::SourceTextEncoding;

    #[test]
    fn cancelled_request_evidence_rejects_a_late_response_before_eof() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(3);
        sender.send(super::OutputEvent::Eof).unwrap();
        drop(sender);
        super::verify_closed_output(&receiver).unwrap();
        for late in [
            super::OutputEvent::Line(br#"{"jsonrpc":"2.0","id":7,"result":{}}"#.to_vec()),
            super::OutputEvent::TooLarge,
            super::OutputEvent::Io(std::io::Error::other("late read error")),
        ] {
            let (sender, receiver) = std::sync::mpsc::sync_channel(3);
            sender.send(late).unwrap();
            sender.send(super::OutputEvent::Eof).unwrap();
            drop(sender);
            assert!(super::verify_closed_output(&receiver).is_err());
        }
    }

    #[test]
    fn physical_oracle_counts_overlaps_and_surrogate_pairs_in_original_bytes() {
        for (encoding, ascii, astral) in [
            (
                SourceTextEncoding::Utf8,
                vec![(0, 3), (2, 5)],
                vec![(6, 12), (11, 17)],
            ),
            (
                SourceTextEncoding::Utf16Le,
                vec![(2, 8), (6, 12)],
                vec![(14, 22), (20, 28)],
            ),
            (
                SourceTextEncoding::Utf16Be,
                vec![(2, 8), (6, 12)],
                vec![(14, 22), (20, 28)],
            ),
        ] {
            let file = WrittenFile {
                path: "source.rs",
                bytes: encode("ababa a\u{1f642}a\u{1f642}a", encoding),
                encoding,
            };
            assert_eq!(expected_spans(&file, "aba"), ascii);
            assert_eq!(expected_spans(&file, "a\u{1f642}a"), astral);
            assert!(expected_spans(&file, "ABA").is_empty());
        }
    }

    #[test]
    fn physical_page_oracle_rejects_self_consistent_omissions_and_wrong_source() {
        let file = WrittenFile {
            path: "src/written.rs",
            bytes: b"ababa".to_vec(),
            encoding: SourceTextEncoding::Utf8,
        };
        let repository = super::RepositoryId::from_bytes([1; 16]);
        let generation = super::GenerationId::from_bytes([2; 20]);
        let file_id = rootlight_ids::FileId::from_bytes([3; 20]);
        let hash = rootlight_ids::content_hash(&file.bytes);
        let index = super::IndexReceipt {
            repository: repository.to_string(),
            generation: generation.to_string(),
            operation: String::new(),
            parent_generation: None,
            syntax_recovery_diagnostic_observed: false,
            status_evidence: super::OperationStatusEvidence {
                label: String::new(),
                state: "published",
                stage: String::new(),
                completed_units: 1,
                total_units: Some(1),
                revision: 1,
                files_examined: 1,
                written_bytes: 5,
                peak_rss_bytes: 0,
                peak_rss_available: false,
                durable_written_bytes_reported: true,
            },
        };
        let source = |start, end| {
            rootlight_ir::SourceRef::new(
                repository,
                generation,
                rootlight_ir::SourceSpan::new(file_id, start, end).unwrap(),
                hash,
                None,
            )
        };
        let reference = serde_json::to_value(source(0, 5)).unwrap();
        let wire = serde_json::json!({
            "schema_version":"1.1",
            "repository":{"repository_id":repository,"display_name":"fixture"},
            "generation":generation,
            "data":{
                "file":{"id":file_id,"path":file.path,"language":"rust",
                    "content_hash":hash,"byte_length":5,"encoding":"utf-8",
                    "generated":false,"trust":"untrusted_repository_data"},
                "matches":[source(0,3),source(2,5)],
                "scanned_start_byte":0,"scanned_end_byte":5,"restoration_bytes":0,
                "stop":"end_of_source","total_metadata_bytes":18,
                "total_response_memory_bytes":512
            },
            "truncated":false,"next_cursor":null,
            "usage":{"rows":3,"edges":0,"source_bytes":0,"json_bytes":512,
                "estimated_tokens":128,"wall_time_ms":1,"cache_status":"not_applicable",
                "trace_id":"physical-oracle"},
            "trust":"untrusted_repository_data"
        });
        let page: super::SourceFindEnvelope = serde_json::from_value(wire).unwrap();
        let verify = |page: &super::SourceFindEnvelope| {
            super::verify_page(page, &index, &file, &reference, "aba", 0, 5)
        };
        assert_eq!(verify(&page).unwrap(), vec![(0, 3), (2, 5)]);
        let mut omitted = page.clone();
        omitted.data.matches.remove(0);
        omitted.validate().unwrap();
        assert!(verify(&omitted).is_err());
        let mut invented = page.clone();
        invented.data.matches = vec![source(1, 4)];
        invented.validate().unwrap();
        assert!(verify(&invented).is_err());
        let mut foreign = page;
        let other = rootlight_ids::FileId::from_bytes([4; 20]);
        foreign.data.file.id = other;
        foreign.data.matches = foreign
            .data
            .matches
            .iter()
            .map(|hit| {
                rootlight_ir::SourceRef::new(
                    repository,
                    generation,
                    rootlight_ir::SourceSpan::new(
                        other,
                        hit.span().start_byte(),
                        hit.span().end_byte(),
                    )
                    .unwrap(),
                    hash,
                    None,
                )
            })
            .collect();
        foreign.validate().unwrap();
        assert!(verify(&foreign).is_err());
    }

    fn storage_index_receipt() -> super::IndexReceipt {
        super::IndexReceipt {
            repository: rootlight_ids::RepositoryId::from_bytes([1; 16]).to_string(),
            generation: rootlight_ids::GenerationId::from_bytes([2; 20]).to_string(),
            parent_generation: Some(rootlight_ids::GenerationId::from_bytes([3; 20]).to_string()),
            operation: rootlight_ids::OperationId::from_bytes([4; 16]).to_string(),
            syntax_recovery_diagnostic_observed: false,
            status_evidence: super::OperationStatusEvidence {
                label: "storage".to_owned(),
                state: "succeeded",
                stage: "complete".to_owned(),
                completed_units: 1,
                total_units: Some(1),
                revision: 1,
                files_examined: 1,
                written_bytes: 1,
                peak_rss_bytes: 0,
                peak_rss_available: false,
                durable_written_bytes_reported: true,
            },
        }
    }

    #[test]
    fn storage_recovery_observation_rejects_unproven_head_and_foreign_status() {
        use serde_json::{Value, json};

        let index = storage_index_receipt();
        let verify = |status: &Value| {
            super::verify_storage_recovery_observation(
                &Value::Null,
                &Value::Null,
                &json!({"response":status}),
                &index,
            )
        };
        let captured = json!({
            "repository": {"repository_id": index.repository},
            "data": {
                "publication_state": "published",
                "repository_state": "ready",
                "resolved_generation": index.generation,
                "active_generation": {
                    "generation_id": index.generation,
                    "parent_generation": index.parent_generation,
                    "semantic_freshness": "stale"
                }
            }
        });
        assert!(
            verify(&captured).is_err(),
            "a head ID needs publication proof"
        );
        let source_only_page = json!({
            "repository": {"repository_id": index.repository},
            "generation": index.parent_generation,
            "data": {"stop": "end_of_source", "matches": []}
        });
        assert!(verify(&source_only_page).is_err());
        assert!(
            verify(&captured).is_err(),
            "a source-only predecessor read cannot update a captured status"
        );
        let mut fresh = captured.clone();
        fresh["data"]["resolved_generation"] = json!(index.parent_generation);
        fresh["data"]["active_generation"]["generation_id"] = json!(index.parent_generation);
        fresh["data"]["active_generation"]["parent_generation"] = Value::Null;
        fresh["data"]["active_generation"]["semantic_freshness"] = json!("current");
        verify(&fresh).unwrap();
        for mutation in 0..9 {
            let mut candidate = captured.clone();
            match mutation {
                0 => candidate["repository"]["repository_id"] = json!("foreign"),
                1 => candidate["data"]["publication_state"] = json!("retained"),
                2 => candidate["data"]["repository_state"] = json!("indexing"),
                3 => candidate["data"]["resolved_generation"] = json!(index.parent_generation),
                4 => candidate["data"]["active_generation"]["generation_id"] = json!("foreign"),
                5 => candidate["data"]["active_generation"]["parent_generation"] = json!("foreign"),
                6 => {
                    candidate["data"]["active_generation"]["semantic_freshness"] = json!("current")
                }
                7 => candidate["data"]["active_generation"] = Value::Null,
                8 => candidate["data"]["resolved_generation"] = Value::Null,
                _ => unreachable!(),
            }
            assert!(verify(&candidate).is_err(), "mutation {mutation}");
        }
        let mut foreign = fresh;
        foreign["repository"]["repository_id"] = json!("foreign");
        assert!(verify(&foreign).is_err());
    }

    #[test]
    fn storage_recovery_requires_quarantine_predecessor_identity_and_new_publication() {
        use serde_json::{Value, json};
        let index = storage_index_receipt();
        let publication = || super::IndexReceipt {
            repository: index.repository.clone(),
            generation: index.generation.clone(),
            parent_generation: index.parent_generation.clone(),
            operation: "recovery-operation".to_owned(),
            syntax_recovery_diagnostic_observed: false,
            status_evidence: index.status_evidence.clone(),
        };
        super::verify_storage_republication_receipt(&index, &publication()).unwrap();
        for mutation in 0..5 {
            let mut candidate = publication();
            match mutation {
                0 => candidate.repository = "foreign-repository".to_owned(),
                1 => candidate.generation = "foreign-generation".to_owned(),
                2 => candidate.parent_generation = Some("foreign-parent".to_owned()),
                3 => candidate.operation = index.operation.clone(),
                4 => candidate.operation.clear(),
                _ => unreachable!(),
            }
            assert!(super::verify_storage_republication_receipt(&index, &candidate).is_err());
        }
        let record = |directory: &str, name: &str, value: Value| {
            let bytes = serde_json::to_vec(&value).unwrap();
            json!({"directory":directory,"name":name,"value":value,
                "hash":rootlight_ids::content_hash(&bytes),"bytes":bytes})
        };
        let manifest = json!({"version":4,"receipt":{"repository":index.repository,
            "generation":index.generation,"parent":index.parent_generation,"elapsed_micros":1}});
        let payload = json!({"repository":index.repository,"generation":index.generation,
            "parent":index.parent_generation,"manifest_hash":"source-tree",
            "configuration_hash":"configuration","provider_set_hash":"providers",
            "schema_version":"1.0","hash":"logical-hash"})
        .to_string();
        let head = record(&index.generation, "manifest.json", manifest.clone());
        let predecessor = record(
            index.parent_generation.as_deref().unwrap(),
            "manifest.json",
            json!({"version":4,"receipt":{"generation":index.parent_generation}}),
        );
        let logical = record(
            &index.generation,
            "logical-snapshot.json",
            json!({
            "digest":rootlight_ids::content_hash(payload.as_bytes()),"payload":payload}),
        );
        let activation = record(
            "activation-2",
            "activation.json",
            json!({
            "generation":index.generation,"global_activation_sequence":2,
            "operation":{"operation":index.operation}}),
        );
        for state in ["corrupt_persisted_state", "incompatible_persisted_schema"] {
            let before = json!({"state":state,"repository":index.repository,"generation":index.generation,
                "original_operation":index.operation,"expected_parent":index.parent_generation,
                "repository_manifests":[head,predecessor,logical,activation],"quarantine_manifests":[]});
            let mut invalid = if state == "corrupt_persisted_state" {
                json!({"directory":index.generation,"name":"manifest.json","value":null,
                    "hash":rootlight_ids::content_hash(b"{"),"bytes":[b'{']})
            } else {
                let mut value = manifest.clone();
                value["version"] = json!(u16::MAX);
                record(&index.generation, "manifest.json", value)
            };
            let mut altered = before.clone();
            altered["repository_manifests"][0] = invalid.clone();
            let mut recovered = before.clone();
            let mut new_manifest = manifest.clone();
            new_manifest["receipt"]["elapsed_micros"] = json!(2);
            recovered["repository_manifests"][0] =
                record(&index.generation, "manifest.json", new_manifest);
            recovered["repository_manifests"][3] = record(
                "activation-3",
                "activation.json",
                json!({
                "generation":index.generation,"global_activation_sequence":3,
                "operation":{"operation":"recovery-operation"}}),
            );
            invalid["directory"] = json!(format!(
                "generation-2-{}-{}-nonce",
                index.repository, index.generation
            ));
            recovered["quarantine_manifests"] = json!([invalid]);
            recovered["response"] = json!({"repository":{"repository_id":index.repository},
                "data":{"publication_state":"published","repository_state":"ready",
                "resolved_generation":index.generation,"active_generation":{"generation_id":index.generation,
                    "parent_generation":index.parent_generation},"logical_snapshot":{"hash":"logical-hash"}}});
            let verify =
                |after: &Value| super::verify_storage_recovery(&before, &altered, after, &index);
            verify(&recovered).unwrap();
            let observe = |after: &Value| {
                super::verify_storage_recovery_observation(&before, &altered, after, &index)
            };
            observe(&recovered).unwrap();
            let mut parent_active = altered.clone();
            parent_active["response"] = recovered["response"].clone();
            parent_active["response"]["data"]["resolved_generation"] =
                json!(index.parent_generation);
            parent_active["response"]["data"]["active_generation"]["generation_id"] =
                json!(index.parent_generation);
            parent_active["response"]["data"]["active_generation"]["parent_generation"] =
                Value::Null;
            observe(&parent_active).unwrap();
            for freshness in ["stale", "current"] {
                let mut published = recovered.clone();
                published["response"]["data"]["active_generation"]["semantic_freshness"] =
                    json!(freshness);
                observe(&published).unwrap();
            }
            for (pointer, value) in [
                ("/quarantine_manifests", json!([])),
                ("/quarantine_manifests/0/bytes", json!([0])),
                ("/repository_manifests/1/hash", json!("changed-predecessor")),
                (
                    "/repository_manifests/0/value/receipt/parent",
                    json!("foreign-parent"),
                ),
                (
                    "/repository_manifests/2/value/payload",
                    json!("changed-provider-identity"),
                ),
                (
                    "/repository_manifests/3/value/operation/operation",
                    json!(index.operation),
                ),
                (
                    "/repository_manifests/3/value/global_activation_sequence",
                    json!(2),
                ),
                (
                    "/response/data/logical_snapshot/hash",
                    json!("foreign-logical-hash"),
                ),
                ("/response/data/publication_state", json!("unpublished")),
                ("/generation", json!("foreign-generation")),
            ] {
                let mut counterexample = recovered.clone();
                *counterexample.pointer_mut(pointer).unwrap() = value;
                assert!(verify(&counterexample).is_err(), "{state}: {pointer}");
                assert!(observe(&counterexample).is_err(), "{state}: {pointer}");
            }
            let mut duplicate = recovered.clone();
            duplicate["repository_manifests"]
                .as_array_mut()
                .unwrap()
                .push(head.clone());
            assert!(verify(&duplicate).is_err());
            assert!(observe(&duplicate).is_err());
            for (directory, generation, sequence) in [
                ("activation-foreign", "foreign-generation", 4),
                ("activation-tie", index.generation.as_str(), 3),
                ("activation-3", index.generation.as_str(), 4),
            ] {
                let mut ambiguous = recovered.clone();
                ambiguous["repository_manifests"]
                    .as_array_mut()
                    .unwrap()
                    .push(record(
                        directory,
                        "activation.json",
                        json!({"generation":generation,"global_activation_sequence":sequence,
                            "operation":{"operation":"foreign-operation"}}),
                    ));
                assert!(observe(&ambiguous).is_err(), "{state}: {directory}");
            }
            let mut identical_valid = recovered.clone();
            identical_valid["repository_manifests"][0] = head.clone();
            verify(&identical_valid).unwrap();
            observe(&identical_valid).unwrap();
            // The live source edit can be reactivated by the watcher before
            // shutdown. Recovery must advance beyond that actual fault baseline.
            let watcher = record(
                "activation-3",
                "activation.json",
                json!({"generation":index.generation,"global_activation_sequence":3,
                    "operation":{"operation":"baseline-watcher-operation"}}),
            );
            let mut watched_before = before.clone();
            watched_before["repository_manifests"]
                .as_array_mut()
                .unwrap()
                .push(watcher.clone());
            let mut watched_altered = altered.clone();
            watched_altered["repository_manifests"]
                .as_array_mut()
                .unwrap()
                .push(watcher);
            let mut watched_recovered = recovered.clone();
            watched_recovered["repository_manifests"][3] = record(
                "activation-4",
                "activation.json",
                json!({"generation":index.generation,"global_activation_sequence":4,
                    "operation":{"operation":"recovery-operation"}}),
            );
            let verify_watched = |before: &Value, altered: &Value, after: &Value| {
                super::verify_storage_recovery(before, altered, after, &index)
            };
            verify_watched(&watched_before, &watched_altered, &watched_recovered).unwrap();
            for (pointer, value) in [
                (
                    "/repository_manifests/3/value/operation/operation",
                    json!("baseline-watcher-operation"),
                ),
                (
                    "/repository_manifests/3/value/operation/operation",
                    json!(index.operation),
                ),
                (
                    "/repository_manifests/3/value/global_activation_sequence",
                    json!(3),
                ),
                ("/repository_manifests/3/directory", json!("activation-3")),
            ] {
                let mut invalid = watched_recovered.clone();
                *invalid.pointer_mut(pointer).unwrap() = value;
                assert!(verify_watched(&watched_before, &watched_altered, &invalid).is_err());
            }
            for pointer in [
                "/repository_manifests/3/value/operation/operation",
                "/repository_manifests/3/value/generation",
            ] {
                let mut invalid = watched_before.clone();
                *invalid.pointer_mut(pointer).unwrap() = json!("foreign-identity");
                let mut invalid_altered = watched_altered.clone();
                *invalid_altered.pointer_mut(pointer).unwrap() = json!("foreign-identity");
                assert!(verify_watched(&invalid, &invalid_altered, &watched_recovered).is_err());
            }
            let mut duplicate_request = watched_before.clone();
            duplicate_request["repository_manifests"][4]["value"]["operation"]["operation"] =
                json!(index.operation);
            let mut duplicate_altered = watched_altered.clone();
            duplicate_altered["repository_manifests"][4]["value"]["operation"]["operation"] =
                json!(index.operation);
            assert!(
                verify_watched(&duplicate_request, &duplicate_altered, &watched_recovered).is_err()
            );
            let mut altered_activation = watched_altered.clone();
            altered_activation["repository_manifests"][4]["hash"] = json!("changed-activation");
            assert!(
                verify_watched(&watched_before, &altered_activation, &watched_recovered).is_err()
            );
            let mut reused_invalid = recovered;
            reused_invalid["repository_manifests"][0] = altered["repository_manifests"][0].clone();
            assert!(verify(&reused_invalid).is_err());
            assert!(observe(&reused_invalid).is_err());
        }
    }

    #[test]
    fn storage_head_selection_binds_operation_and_latest_publication_sequence() {
        use serde_json::{Value, json};
        let activation = |directory: &str, generation: &str, sequence: u64, operation: &str| {
            json!({"directory":directory,"name":"activation.json","value":{
                "generation":generation,"global_activation_sequence":sequence,
                "operation":{"operation":operation}}})
        };
        let snapshot = json!({"repository_manifests":[
            activation("activation-1", "predecessor", 1, "base-operation"),
            activation("activation-2", "head", 2, "first-operation"),
            activation("activation-3", "head", 3, "requested-operation"),
        ]});
        let selected =
            super::head_activation(&snapshot, "head", Some("requested-operation")).unwrap();
        assert_eq!(selected["directory"], "activation-3");
        assert_eq!(
            super::head_activation(&snapshot, "head", None).unwrap(),
            selected
        );
        assert!(super::head_activation(&snapshot, "head", Some("first-operation")).is_err());
        assert!(super::head_activation(&snapshot, "head", Some("missing-operation")).is_err());
        for extra in [
            activation("activation-3", "head", 3, "requested-operation"),
            activation("activation-tie", "head", 3, "different-operation"),
            activation("activation-4", "foreign", 4, "different-operation"),
            activation("activation-0", "head", 0, "different-operation"),
            activation("activation-repeat", "head", 4, "requested-operation"),
        ] {
            let mut invalid = snapshot.clone();
            invalid["repository_manifests"]
                .as_array_mut()
                .unwrap()
                .push(extra);
            assert!(super::head_activation(&invalid, "head", Some("requested-operation")).is_err());
        }
        for pointer in [
            "/repository_manifests/2/value/global_activation_sequence",
            "/repository_manifests/2/value/generation",
            "/repository_manifests/2/value/operation/operation",
            "/repository_manifests/2/directory",
        ] {
            let mut invalid = snapshot.clone();
            *invalid.pointer_mut(pointer).unwrap() = Value::Null;
            assert!(super::head_activation(&invalid, "head", Some("requested-operation")).is_err());
        }
    }

    #[test]
    fn process_fixture_has_literal_only_and_overlapping_authored_obligations() {
        for file in files().iter().take(3) {
            assert_eq!(expected_spans(file, "needlefragment").len(), 1);
            assert_eq!(expected_spans(file, "aba").len(), 2);
            assert_eq!(expected_spans(file, "a\u{1f642}a").len(), 2);
            assert!(expected_spans(file, "NeedleFragment").is_empty());
        }
    }
}
