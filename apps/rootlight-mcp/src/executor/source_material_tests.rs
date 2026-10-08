//! Second-stage source materialization through deterministic prefix-port fixtures.
//! Scalar/physical oracles are independent of the production mapping helpers;
//! failures retain already received work without claiming outer-select accounting.

use super::*;
use rootlight_agent::context_evidence::{
    ContextEvidenceCollector, ContextEvidenceProviderRegistry, ContextSourceOutput,
    ContextSourceRequest, ContextSourceTarget, EvidenceCandidateId,
};
use rootlight_mcp_contract::context::SourcePolicy;

pub(super) fn prefix_legacy_fixture(
    legacy: SourceReadPortResponse,
    request: &SourceReadPortRequest,
    maximum: u32,
) -> Result<client::SourceTextPrefixReadResult, ClientPortError> {
    let chunks = legacy
        .result
        .chunks
        .into_iter()
        .map(|chunk| {
            if chunk.encoding != client::SourceEncoding::Utf8 {
                return Err(ClientPortError::InvalidResponse);
            }
            Ok(client::SourceTextChunk {
                extracted_from: chunk.extracted_from,
                source: chunk.source,
                path: chunk.path,
                start_byte: chunk.start_byte,
                end_byte: chunk.end_byte,
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                content: String::from_utf8(chunk.content)
                    .map_err(|_| ClientPortError::InvalidResponse)?,
                physical_encoding: client::SourceTextEncoding::Utf8,
                content_hash: chunk.content_hash,
                language: chunk.language,
                tier: chunk.tier,
                generated: chunk.generated,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    prefix_text_fixture(
        client::SourceTextReadResult {
            context: legacy.result.context,
            chunks,
            total_source_bytes: legacy.result.total_source_bytes,
            total_text_bytes: legacy.result.total_source_bytes,
            truncated: legacy.result.truncated,
            execution_completeness: legacy.result.execution_completeness,
            conditional_activity: legacy.result.conditional_activity,
        },
        request,
        maximum,
    )
}

pub(super) fn prefix_text_fixture(
    mut result: client::SourceTextReadResult,
    request: &SourceReadPortRequest,
    maximum: u32,
) -> Result<client::SourceTextPrefixReadResult, ClientPortError> {
    let maximum = usize::try_from(maximum).map_err(|_| ClientPortError::InvalidResponse)?;
    let mut prefix_truncated = false;
    let mut physical = 0_u64;
    let mut text = 0_u64;
    for chunk in &mut result.chunks {
        let end = scalar_prefix_end(&chunk.content, maximum);
        prefix_truncated |= end < chunk.content.len();
        chunk.content.truncate(end);
        let width = physical_width(&chunk.content, chunk.physical_encoding);
        chunk.end_byte = chunk
            .start_byte
            .checked_add(width)
            .ok_or(ClientPortError::InvalidResponse)?;
        if request.include_line_numbers() {
            let newlines = chunk
                .content
                .chars()
                .filter(|character| *character == '\n')
                .count()
                .saturating_sub(usize::from(chunk.content.ends_with('\n')));
            chunk.end_line = chunk
                .start_line
                .map(|start| {
                    start
                        .checked_add(u64::try_from(newlines).unwrap())
                        .ok_or(ClientPortError::InvalidResponse)
                })
                .transpose()?;
        } else {
            chunk.start_line = None;
            chunk.end_line = None;
        }
        physical = physical
            .checked_add(width)
            .ok_or(ClientPortError::InvalidResponse)?;
        text = text
            .checked_add(u64::try_from(chunk.content.len()).unwrap())
            .ok_or(ClientPortError::InvalidResponse)?;
    }
    result.total_source_bytes = physical;
    result.total_text_bytes = text;
    result.context.usage.results = u64::try_from(result.chunks.len()).unwrap();
    result.context.usage.source_bytes = physical;
    result.context.usage.estimated_tokens =
        rootlight_mcp_contract::accounting::estimate_tokens(usize::try_from(text).unwrap());
    Ok(client::SourceTextPrefixReadResult {
        result,
        prefix_truncated,
    })
}

fn scalar_prefix_end(text: &str, maximum: usize) -> usize {
    text.char_indices()
        .map(|(offset, character)| offset + character.len_utf8())
        .take_while(|end| *end <= maximum)
        .last()
        .unwrap_or(0)
}

fn physical_width(text: &str, encoding: client::SourceTextEncoding) -> u64 {
    u64::try_from(match encoding {
        client::SourceTextEncoding::Utf8 => text.len(),
        client::SourceTextEncoding::Utf16Le | client::SourceTextEncoding::Utf16Be => {
            text.encode_utf16().count() * 2
        }
    })
    .unwrap()
}

fn physical_bytes(text: &str, encoding: client::SourceTextEncoding) -> Vec<u8> {
    match encoding {
        client::SourceTextEncoding::Utf8 => text.as_bytes().to_vec(),
        client::SourceTextEncoding::Utf16Le | client::SourceTextEncoding::Utf16Be => text
            .encode_utf16()
            .flat_map(|unit| match encoding {
                client::SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
                client::SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
                client::SourceTextEncoding::Utf8 => unreachable!(),
            })
            .collect(),
    }
}

fn codec(encoding: client::SourceTextEncoding) -> Option<rootlight_ir::SourceTextEncoding> {
    match encoding {
        client::SourceTextEncoding::Utf8 => None,
        client::SourceTextEncoding::Utf16Le => Some(rootlight_ir::SourceTextEncoding::Utf16Le),
        client::SourceTextEncoding::Utf16Be => Some(rootlight_ir::SourceTextEncoding::Utf16Be),
    }
}

pub(super) async fn candidate_id() -> EvidenceCandidateId {
    let input: ContextPackInput = serde_json::from_value(json!({
        "repository": {"repository_id": repository()}, "task": "inspect selected source",
        "seeds": {"symbols": [symbol()]}, "sections": ["definitions"],
        "source_policy": "references_only", "token_budget": 4000,
    }))
    .unwrap();
    let request = CanonicalContextPackRequest::new(&input, repository(), generation()).unwrap();
    let plan = ContextEvidenceProviderRegistry.plan(&request).unwrap();
    let mut explain = explain_response(source_reference_without_lines(4, 12));
    explain.result.unresolved_symbols.clear();
    explain.result.context.usage.results = 1;
    let harness = Harness::new(FakeOutcome::SymbolExplain(Ok(explain)));
    let adapter = McpAgentToolPort {
        port: Arc::clone(&harness.executor.port),
        validator: Arc::clone(&harness.executor.batch_validator),
        unsupported: harness.executor.unsupported.clone(),
        invalid_arguments: harness.executor.invalid_arguments.clone(),
        invalid_cursor: harness.executor.invalid_cursor.clone(),
        exposure_profile: ExposureProfile::Developer,
        cursor_key: harness.executor.cursor_key,
    };
    let corpus = ContextEvidenceCollector
        .collect(
            &adapter,
            &request,
            &plan,
            cancellation(),
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .unwrap();
    corpus
        .candidates
        .first()
        .expect("definition fixture yields a typed candidate")
        .id()
        .clone()
}

fn request(
    references: &[client::SourceReference],
    candidate: &EvidenceCandidateId,
    snippets: bool,
    maximum: u32,
) -> ContextSourceRequest {
    ContextSourceRequest {
        repository: repository(),
        generation: generation(),
        source_policy: if snippets {
            SourcePolicy::FocusedSnippets
        } else {
            SourcePolicy::Signatures
        },
        include_snippets: snippets,
        max_bytes_per_snippet: maximum,
        targets: references
            .iter()
            .map(|reference| ContextSourceTarget {
                candidate_id: candidate.clone(),
                source_ref: client_source_ref(reference).unwrap(),
            })
            .collect(),
    }
}

fn reservation() -> BudgetCharge {
    BudgetLimits::server_ceiling().maximums()
}

fn harness(
    responses: Vec<Result<client::SourceTextPrefixReadResult, ClientPortError>>,
    cancel: Option<watch::Sender<bool>>,
) -> Harness {
    Harness::new(FakeOutcome::SourceReadPrefixSequence {
        responses: Arc::new(Mutex::new(responses.into())),
        cancel_on_last_response: cancel,
    })
}

async fn materialize(
    harness: &Harness,
    request: ContextSourceRequest,
    cancellation: RequestCancellation,
) -> Result<ContextSourceOutput, ContextEvidencePortError> {
    context_evidence::materialize_context_source(
        Arc::clone(&harness.executor.port),
        request,
        reservation(),
        cancellation,
    )
    .await
}

async fn materialize_adapter(
    harness: &Harness,
    request: ContextSourceRequest,
) -> Result<ContextSourceOutput, ContextEvidencePortError> {
    let adapter = McpAgentToolPort {
        port: Arc::clone(&harness.executor.port),
        validator: Arc::clone(&harness.executor.batch_validator),
        unsupported: harness.executor.unsupported.clone(),
        invalid_arguments: harness.executor.invalid_arguments.clone(),
        invalid_cursor: harness.executor.invalid_cursor.clone(),
        exposure_profile: ExposureProfile::Developer,
        cursor_key: harness.executor.cursor_key,
    };
    adapter
        .materialize_source(
            request,
            ContextEvidenceCallContext::new(
                cancellation(),
                Instant::now() + Duration::from_secs(10),
                reservation(),
            ),
        )
        .await
}

fn fixture(
    encoding: client::SourceTextEncoding,
    ordinal: u8,
    maximum: usize,
    expanded: bool,
) -> client::SourceTextPrefixReadResult {
    let text = "\u{feff}head\u{010a}\r\nA\u{2603}\u{1f642}B\r\nend\r\ntail";
    let selection_start = text.find('A').unwrap();
    let selection_end = text.find("B\r\n").unwrap() + 1;
    let raw = physical_bytes(text, encoding);
    let hash = rootlight_ids::content_hash(&raw);
    let source = client::SourceReference::new(
        repository(),
        generation(),
        FileId::from_bytes([ordinal; 20]),
        physical_width(&text[..selection_start], encoding)
            ..physical_width(&text[..selection_end], encoding),
        hash,
        None,
    )
    .unwrap();
    let start = if expanded { 0 } else { selection_start };
    let end = if expanded {
        text.find("tail").unwrap()
    } else {
        selection_end
    };
    let full = &text[start..end];
    let end = scalar_prefix_end(full, maximum);
    let content = &full[..end];
    let physical_start = physical_width(&text[..start], encoding);
    let physical = physical_width(content, encoding);
    let mut context = context(1, physical);
    context.usage.memory_bytes = Some(u64::try_from(content.len() + 64).unwrap());
    context.usage.json_bytes = u64::try_from(content.len() + 128).unwrap();
    context.usage.rows = 17;
    client::SourceTextPrefixReadResult {
        result: client::SourceTextReadResult {
            context,
            chunks: vec![client::SourceTextChunk {
                extracted_from: None,
                source,
                path: format!("src/item-{ordinal}.note"),
                start_byte: physical_start,
                end_byte: physical_start + physical,
                start_line: None,
                end_line: None,
                content: content.to_owned(),
                physical_encoding: encoding,
                content_hash: hash,
                language: "unknown".into(),
                tier: ClientTier::TierB,
                generated: false,
            }],
            total_source_bytes: physical,
            total_text_bytes: u64::try_from(content.len()).unwrap(),
            truncated: false,
            execution_completeness: complete_execution(),
            conditional_activity: Vec::new(),
        },
        prefix_truncated: end < full.len(),
    }
}

fn joined(
    responses: Vec<client::SourceTextPrefixReadResult>,
) -> client::SourceTextPrefixReadResult {
    let mut output = responses[0].clone();
    for response in responses.into_iter().skip(1) {
        let old = output.result.context.usage;
        let next = response.result.context.usage;
        output.result.context.usage.rows = old.rows + next.rows;
        output.result.context.usage.edges = old.edges + next.edges;
        output.result.context.usage.json_bytes = old.json_bytes + next.json_bytes;
        output.result.context.usage.estimated_tokens = old.estimated_tokens + next.estimated_tokens;
        output.result.context.usage.memory_bytes =
            Some(old.memory_bytes.unwrap_or(0) + next.memory_bytes.unwrap_or(0));
        output.result.context.usage.elapsed_micros = old.elapsed_micros.max(next.elapsed_micros);
        output.result.chunks.extend(response.result.chunks);
        output.result.total_source_bytes += response.result.total_source_bytes;
        output.result.total_text_bytes += response.result.total_text_bytes;
        output.prefix_truncated |= response.prefix_truncated;
    }
    output.result.context.usage.results = u64::try_from(output.result.chunks.len()).unwrap();
    output.result.context.usage.source_bytes = output.result.total_source_bytes;
    output
}

#[tokio::test]
async fn second_stage_source_preserves_full_authority_context_and_all_codecs() {
    let candidate = candidate_id().await;
    for encoding in [
        client::SourceTextEncoding::Utf8,
        client::SourceTextEncoding::Utf16Le,
        client::SourceTextEncoding::Utf16Be,
    ] {
        let response = fixture(encoding, 11, 1024, true);
        let chunk = response.result.chunks[0].clone();
        let original = client_source_ref(&chunk.source).unwrap();
        let expected_usage = first_stage_known_work(&[&response.result.context]);
        let harness = harness(vec![Ok(response)], None);
        let output = materialize_adapter(
            &harness,
            request(std::slice::from_ref(&chunk.source), &candidate, true, 1024),
        )
        .await
        .unwrap();
        assert_eq!(
            (output.repository, output.generation),
            (repository(), generation())
        );
        assert_eq!(output.usage, expected_usage);
        assert_eq!(output.completeness.state, CompletenessState::Complete);
        let [material] = output.materials.as_slice() else {
            panic!("one selected material is returned");
        };
        assert_eq!(material.candidate_id, candidate);
        assert_eq!(material.source_ref, original);
        assert_eq!(material.source_ref.content_hash(), chunk.content_hash);
        assert_eq!(material.signature.as_deref(), Some("A\u{2603}\u{1f642}B"));
        assert_eq!(material.projection.start_byte, chunk.start_byte);
        assert_eq!(material.projection.end_byte, chunk.end_byte);
        assert_eq!(material.projection.encoding, codec(encoding));
        assert_eq!(
            material.projection.text_bytes,
            u64::try_from(chunk.content.len()).unwrap()
        );
        let snippet = material.snippet.as_ref().unwrap();
        assert_eq!(snippet.content, chunk.content);
        assert_eq!(snippet.language, "unknown");
        assert!(!snippet.truncated);
        assert_eq!(
            physical_bytes(&snippet.content, encoding).len(),
            usize::try_from(output.usage.source_bytes).unwrap()
        );
        assert!(material.projection.start_byte < original.span().start_byte());
        assert!(material.projection.end_byte > original.span().end_byte());
        let ObservedCall::SourceReadLosslessPrefix {
            call,
            maximum_text_bytes,
        } = harness.only_call()
        else {
            panic!("second-stage source uses the prefix port");
        };
        assert_eq!(maximum_text_bytes, 1024);
        assert_eq!(call.references(), [chunk.source]);
        assert_eq!(
            (call.context_lines_before(), call.context_lines_after()),
            (1, 1)
        );
        assert!(!call.include_line_numbers());
        assert!(!call.merge_overlaps());
    }
}

#[tokio::test]
async fn second_stage_source_clips_scalars_and_omits_context_only_prefixes() {
    let candidate = candidate_id().await;
    for encoding in [
        client::SourceTextEncoding::Utf8,
        client::SourceTextEncoding::Utf16Le,
        client::SourceTextEncoding::Utf16Be,
    ] {
        let complete = fixture(encoding, 12, 1024, true);
        let original = complete.result.chunks[0].source.clone();
        let selection_start = "\u{feff}head\u{010a}\r\n".len();
        let selection_end = selection_start + "A\u{2603}\u{1f642}B".len();
        for maximum in 1..=complete.result.chunks[0].content.len() + 1 {
            let response = fixture(encoding, 12, maximum, true);
            let chunk = response.result.chunks[0].clone();
            let truncated = response.prefix_truncated;
            let expected_usage = first_stage_known_work(&[&response.result.context]);
            let harness = harness(vec![Ok(response)], None);
            let output = materialize(
                &harness,
                request(
                    std::slice::from_ref(&original),
                    &candidate,
                    true,
                    u32::try_from(maximum).unwrap(),
                ),
                cancellation(),
            )
            .await
            .unwrap();
            assert_eq!(output.usage, expected_usage);
            assert_eq!(chunk.source, original);
            if chunk.content.len() <= selection_start {
                assert!(
                    output.materials.is_empty(),
                    "leading context is not selected evidence"
                );
                assert_eq!(output.completeness.state, CompletenessState::Truncated);
                assert!(
                    output
                        .completeness
                        .limiting_resources
                        .iter()
                        .any(|resource| resource.kind == ContractLimitingResourceKind::SourceBytes)
                );
            } else {
                let [material] = output.materials.as_slice() else {
                    panic!("visible selected prefix is materialized");
                };
                assert_eq!(material.source_ref, client_source_ref(&original).unwrap());
                assert_eq!(
                    material.signature.as_deref(),
                    Some(&chunk.content[selection_start..chunk.content.len().min(selection_end)])
                );
                assert_eq!(material.projection.encoding, codec(encoding));
                assert_eq!(material.projection.end_byte, chunk.end_byte);
                assert_eq!(
                    material.projection.text_bytes,
                    u64::try_from(chunk.content.len()).unwrap()
                );
                let snippet = material.snippet.as_ref().unwrap();
                assert_eq!(snippet.content, chunk.content);
                assert_eq!(snippet.truncated, truncated);
                assert_eq!(
                    physical_width(&snippet.content, encoding),
                    output.usage.source_bytes
                );
                assert_eq!(output.completeness.state, CompletenessState::Complete);
            }
        }
    }
}

#[tokio::test]
async fn second_stage_source_signature_only_uses_zero_context_without_body_leak() {
    let candidate = candidate_id().await;
    for encoding in [
        client::SourceTextEncoding::Utf8,
        client::SourceTextEncoding::Utf16Le,
        client::SourceTextEncoding::Utf16Be,
    ] {
        for maximum in 1..=9 {
            let response = fixture(encoding, 13, maximum, false);
            let truncated = response.prefix_truncated;
            let work = first_stage_known_work(&[&response.result.context]);
            let chunk = response.result.chunks[0].clone();
            let harness = harness(vec![Ok(response)], None);
            let output = materialize(
                &harness,
                request(
                    std::slice::from_ref(&chunk.source),
                    &candidate,
                    false,
                    u32::try_from(maximum).unwrap(),
                ),
                cancellation(),
            )
            .await
            .unwrap();
            assert_eq!(output.usage, work);
            assert_eq!(
                output.completeness.state,
                if truncated {
                    CompletenessState::Truncated
                } else {
                    CompletenessState::Complete
                }
            );
            if truncated {
                assert!(
                    output
                        .completeness
                        .limiting_resources
                        .iter()
                        .any(|resource| resource.kind == ContractLimitingResourceKind::SourceBytes)
                );
            } else {
                assert!(output.completeness.limiting_resources.is_empty());
            }
            let [material] = output.materials.as_slice() else {
                panic!("the initial ASCII scalar fits every positive ceiling");
            };
            assert_eq!(
                material.source_ref,
                client_source_ref(&chunk.source).unwrap()
            );
            assert_eq!(material.signature.as_deref(), Some(chunk.content.as_str()));
            assert!(material.snippet.is_none());
            assert_eq!(material.projection.encoding, codec(encoding));
            assert_eq!(
                material.projection.start_byte,
                material.source_ref.span().start_byte()
            );
            let ObservedCall::SourceReadLosslessPrefix { call, .. } = harness.only_call() else {
                panic!("signature-only uses the prefix port");
            };
            assert_eq!(
                (call.context_lines_before(), call.context_lines_after()),
                (0, 0)
            );
        }
    }
}

fn whitespace_fixture(
    encoding: client::SourceTextEncoding,
    expanded: bool,
) -> client::SourceTextPrefixReadResult {
    let text = "\u{feff}head\r\n \t\r\nend\r\ntail";
    let start = text.find(" \t").unwrap();
    let end = start + " \t".len();
    let raw = physical_bytes(text, encoding);
    let hash = rootlight_ids::content_hash(&raw);
    let mut response = fixture(encoding, 16, 1024, expanded);
    let chunk = &mut response.result.chunks[0];
    chunk.source = client::SourceReference::new(
        repository(),
        generation(),
        chunk.source.file(),
        physical_width(&text[..start], encoding)..physical_width(&text[..end], encoding),
        hash,
        None,
    )
    .unwrap();
    let content = if expanded {
        &text[..text.find("tail").unwrap()]
    } else {
        &text[start..end]
    };
    chunk.content = content.to_owned();
    chunk.content_hash = hash;
    chunk.start_byte = if expanded {
        0
    } else {
        physical_width(&text[..start], encoding)
    };
    chunk.end_byte = chunk.start_byte + physical_width(content, encoding);
    response.result.total_source_bytes = physical_width(content, encoding);
    response.result.total_text_bytes = u64::try_from(content.len()).unwrap();
    response.result.context.usage.source_bytes = response.result.total_source_bytes;
    response.result.context.usage.memory_bytes = Some(u64::try_from(content.len() + 64).unwrap());
    response.result.context.usage.json_bytes = u64::try_from(content.len() + 128).unwrap();
    response.prefix_truncated = false;
    response
}

#[tokio::test]
async fn second_stage_source_whitespace_only_approved_selection_omits_material_and_preserves_work()
{
    let candidate = candidate_id().await;
    for encoding in [
        client::SourceTextEncoding::Utf8,
        client::SourceTextEncoding::Utf16Le,
        client::SourceTextEncoding::Utf16Be,
    ] {
        for snippets in [false, true] {
            let response = whitespace_fixture(encoding, snippets);
            let reference = response.result.chunks[0].source.clone();
            let work = first_stage_known_work(&[&response.result.context]);
            assert!(
                !response.prefix_truncated,
                "the complete selection is whitespace, not clipped"
            );
            let harness = harness(vec![Ok(response)], None);
            let output = materialize(
                &harness,
                request(std::slice::from_ref(&reference), &candidate, snippets, 1024),
                cancellation(),
            )
            .await
            .unwrap();
            assert!(
                output.materials.is_empty(),
                "context outside an empty declaration is not selected evidence"
            );
            assert_eq!(output.usage, work);
            assert!(output.usage.source_bytes > 0);
            assert_eq!(output.completeness.state, CompletenessState::Truncated);
            assert!(
                output
                    .completeness
                    .limiting_resources
                    .iter()
                    .any(|resource| resource.kind == ContractLimitingResourceKind::SourceBytes)
            );
            let ObservedCall::SourceReadLosslessPrefix { call, .. } = harness.only_call() else {
                panic!("whitespace evidence uses the prefix port");
            };
            assert_eq!(call.references(), [reference]);
            assert_eq!(
                (call.context_lines_before(), call.context_lines_after()),
                if snippets { (1, 1) } else { (0, 0) }
            );
        }
    }
}

#[tokio::test]
async fn second_stage_source_rejects_geometry_codec_totals_order_and_completeness_without_losing_work()
 {
    let candidate = candidate_id().await;
    for encoding in [
        client::SourceTextEncoding::Utf8,
        client::SourceTextEncoding::Utf16Le,
        client::SourceTextEncoding::Utf16Be,
    ] {
        let valid = joined(vec![
            fixture(encoding, 14, 1024, true),
            fixture(encoding, 15, 1024, true),
        ]);
        let references: Vec<_> = valid
            .result
            .chunks
            .iter()
            .map(|chunk| chunk.source.clone())
            .collect();
        let maximum = u32::try_from(valid.result.chunks[0].content.len()).unwrap();
        for mutation in 0..15 {
            let mut invalid = valid.clone();
            match mutation {
                0 => invalid.result.context.generation = alternate_generation(),
                1 => invalid.result.chunks[0].end_byte += 1,
                2 => {
                    invalid.result.chunks[0].physical_encoding =
                        if encoding == client::SourceTextEncoding::Utf8 {
                            client::SourceTextEncoding::Utf16Le
                        } else {
                            client::SourceTextEncoding::Utf8
                        }
                }
                3 => invalid.result.total_source_bytes += 1,
                4 => invalid.result.total_text_bytes += 1,
                5 => invalid.result.context.usage.source_bytes += 1,
                6 => invalid.result.chunks.swap(0, 1),
                7 => invalid.result.chunks[0].source = references[1].clone(),
                8 => invalid.result.chunks[0].content_hash = ContentHash::from_bytes([9; 32]),
                9 => invalid.result.chunks[0].start_line = Some(1),
                10 => invalid.result.context.usage.results -= 1,
                11 => invalid.result.chunks[0].content.push('x'),
                12 => invalid.result.context.parent_generation = Some(generation()),
                13 => invalid.result.truncated = true,
                _ => {
                    invalid.result.chunks.clear();
                    invalid.result.context.usage.results = 0;
                    invalid.result.context.usage.source_bytes = 0;
                    invalid.result.total_source_bytes = 0;
                    invalid.result.total_text_bytes = 0;
                }
            }
            let work = first_stage_known_work(&[&invalid.result.context]);
            let harness = harness(vec![Ok(invalid)], None);
            let error = materialize(
                &harness,
                request(&references, &candidate, true, maximum),
                cancellation(),
            )
            .await
            .expect_err("inconsistent prefix transport cannot produce editing evidence");
            assert_eq!(
                error.kind,
                ContextEvidencePortErrorKind::InvalidResponse,
                "mutation {mutation}"
            );
            assert_eq!(
                error.usage, work,
                "received work survives mutation {mutation}"
            );
            assert_eq!(harness.call_count.load(Ordering::Relaxed), 1);
        }
        let mut invalid = fixture(encoding, 14, 1, true);
        invalid.prefix_truncated = false;
        let work = first_stage_known_work(&[&invalid.result.context]);
        let harness = harness(vec![Ok(invalid)], None);
        let error = materialize(
            &harness,
            request(&references[..1], &candidate, true, 1),
            cancellation(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ContextEvidencePortErrorKind::InvalidResponse);
        assert_eq!(
            error.usage, work,
            "a missing prefix flag cannot hide omitted approved bytes"
        );
    }
}

fn batches(
    encoding: client::SourceTextEncoding,
) -> (
    Vec<client::SourceReference>,
    Vec<client::SourceTextPrefixReadResult>,
) {
    let single: Vec<_> = (1..=65)
        .map(|ordinal| fixture(encoding, ordinal, 1024, false))
        .collect();
    let references = single
        .iter()
        .map(|response| response.result.chunks[0].source.clone())
        .collect();
    let responses = single
        .chunks(32)
        .map(|batch| joined(batch.to_vec()))
        .collect();
    (references, responses)
}

fn assert_batch_calls(
    harness: &Harness,
    references: &[client::SourceReference],
    responses: &[client::SourceTextPrefixReadResult],
) {
    let calls = harness
        .calls
        .lock()
        .expect("fake prefix call recorder is not poisoned");
    assert_eq!(calls.len(), responses.len());
    let mut prior = BudgetCharge::default();
    for (index, (call, response)) in calls.iter().zip(responses).enumerate() {
        let ObservedCall::SourceReadLosslessPrefix {
            call,
            maximum_text_bytes,
        } = call
        else {
            panic!("materialization does not use an exact legacy route");
        };
        assert_eq!(*maximum_text_bytes, 1024);
        let start = index * 32;
        assert_eq!(
            call.references(),
            &references[start..(start + 32).min(references.len())]
        );
        assert!(call.references().len() <= 32);
        assert_eq!(
            (call.context_lines_before(), call.context_lines_after()),
            (0, 0)
        );
        let limits = call.options.effective_budget().unwrap().limits();
        assert_eq!(limits.rows, reservation().rows - prior.rows);
        assert_eq!(
            limits.source_bytes,
            reservation().source_bytes - prior.source_bytes
        );
        assert_eq!(
            limits.json_bytes,
            reservation().json_bytes - prior.json_bytes
        );
        assert_eq!(
            limits.memory_bytes,
            reservation().memory_bytes - prior.memory_bytes
        );
        prior.rows += response.result.context.usage.rows;
        prior.source_bytes += response.result.context.usage.source_bytes;
        prior.json_bytes += response.result.context.usage.json_bytes;
        prior.memory_bytes += response.result.context.usage.memory_bytes.unwrap_or(0);
    }
}

// Repeated opaque candidate IDs isolate adapter reference batching and receipts.
// This does not exercise the agent's distinct-candidate admission contract.
#[tokio::test]
async fn second_stage_source_batches_over_selector_limit_in_order_with_remaining_work_budget() {
    let candidate = candidate_id().await;
    for encoding in [
        client::SourceTextEncoding::Utf8,
        client::SourceTextEncoding::Utf16Le,
        client::SourceTextEncoding::Utf16Be,
    ] {
        let (references, responses) = batches(encoding);
        let contexts: Vec<_> = responses
            .iter()
            .map(|response| &response.result.context)
            .collect();
        let expected = first_stage_known_work(&contexts);
        let harness = harness(responses.iter().cloned().map(Ok).collect(), None);
        let output = materialize(
            &harness,
            request(&references, &candidate, false, 1024),
            cancellation(),
        )
        .await
        .unwrap();
        assert_eq!(output.usage, expected);
        assert_eq!(output.completeness.state, CompletenessState::Complete);
        assert_eq!(output.materials.len(), references.len());
        for (material, source) in output.materials.iter().zip(&references) {
            assert_eq!(material.candidate_id, candidate);
            assert_eq!(material.source_ref, client_source_ref(source).unwrap());
            assert_eq!(material.projection.encoding, codec(encoding));
            assert_eq!(material.signature.as_deref(), Some("A\u{2603}\u{1f642}B"));
            assert!(material.snippet.is_none());
        }
        assert_batch_calls(&harness, &references, &responses);
    }
}

#[tokio::test]
async fn second_stage_source_later_batch_failure_preserves_earlier_received_work() {
    let candidate = candidate_id().await;
    let (references, responses) = batches(client::SourceTextEncoding::Utf16Le);
    for failure in [ClientPortError::Transport, ClientPortError::InvalidResponse] {
        let expected = first_stage_known_work(&[&responses[0].result.context]);
        let harness = harness(vec![Ok(responses[0].clone()), Err(failure.clone())], None);
        let error = materialize(
            &harness,
            request(&references, &candidate, false, 1024),
            cancellation(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.usage, expected);
        assert_eq!(
            error.kind,
            if matches!(failure, ClientPortError::InvalidResponse) {
                ContextEvidencePortErrorKind::InvalidResponse
            } else {
                ContextEvidencePortErrorKind::Unavailable
            }
        );
        assert_batch_calls(&harness, &references, &responses[..2]);
    }
    let mut invalid = responses[2].clone();
    invalid.result.context.generation = alternate_generation();
    let expected = first_stage_known_work(&[
        &responses[0].result.context,
        &responses[1].result.context,
        &invalid.result.context,
    ]);
    let harness = harness(
        vec![
            Ok(responses[0].clone()),
            Ok(responses[1].clone()),
            Ok(invalid),
        ],
        None,
    );
    let error = materialize(
        &harness,
        request(&references, &candidate, false, 1024),
        cancellation(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ContextEvidencePortErrorKind::InvalidResponse);
    assert_eq!(
        error.usage, expected,
        "just-received invalid last-batch work is not erased"
    );
    assert_batch_calls(&harness, &references, &responses);
}

#[tokio::test]
async fn second_stage_source_later_batch_cancellation_retains_completed_receipts() {
    let candidate = candidate_id().await;
    let (references, responses) = batches(client::SourceTextEncoding::Utf16Be);
    let (sender, receiver) = watch::channel(false);
    let expected =
        first_stage_known_work(&[&responses[0].result.context, &responses[1].result.context]);
    let harness = harness(
        vec![Ok(responses[0].clone()), Ok(responses[1].clone())],
        Some(sender),
    );
    let error = materialize(
        &harness,
        request(&references, &candidate, false, 1024),
        RequestCancellation { receiver },
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ContextEvidencePortErrorKind::Cancelled);
    assert_eq!(error.usage, expected);
    assert_batch_calls(&harness, &references, &responses[..2]);
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        2,
        "no third batch starts after cancellation"
    );
}
