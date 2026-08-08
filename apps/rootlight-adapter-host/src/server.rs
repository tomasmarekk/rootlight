//! Bounded child-side transport for one project analysis request.
//!
//! The isolated process accepts exactly one length-prefixed frame and emits
//! exactly one correlated result. Semantic analysis remains supplied by a
//! caller so the transport contract can be tested independently.

use std::io::{Read, Write};

use rootlight_cancel::Cancellation;
use rootlight_ids::content_hash;
use rootlight_protocol::{
    adapter_contract::{
        AdapterFrameDecoder, MAX_ADAPTER_FRAME_BYTES, MAX_ADAPTER_PROJECT_OUTPUT_BYTES,
        encode_length_delimited_adapter_frame,
    },
    generated::adapter::v1::{
        AdapterFrame, ProjectAnalysisRequest, ProjectAnalysisResult, ProjectAnalysisResultChunk,
        ProjectAnalysisResultEnd, adapter_frame,
    },
};

use crate::AdapterHostError;

const PIPE_CHUNK_BYTES: usize = 16 * 1024;
const RESULT_CHUNK_BYTES: usize = MAX_ADAPTER_FRAME_BYTES - 1024;

/// Serves one exact bounded project frame on an isolated child pipe.
///
/// `handler` receives only a protocol-decoded request. Its returned result must
/// repeat both correlation identities and authenticate its normalized IR bytes.
/// The function writes no partial response when validation fails.
///
/// # Errors
///
/// Returns [`AdapterHostError`] for cancellation, malformed or trailing input,
/// correlation or digest substitution, handler failure, output overflow, or a
/// pipe failure.
pub fn serve_project_session<R, W, H>(
    reader: &mut R,
    writer: &mut W,
    cancellation: &Cancellation,
    handler: H,
) -> Result<(), AdapterHostError>
where
    R: Read,
    W: Write,
    H: FnOnce(
        ProjectAnalysisRequest,
        &Cancellation,
    ) -> Result<ProjectAnalysisResult, AdapterHostError>,
{
    cancellation.check()?;
    let frame = read_exact_frame(reader, cancellation)?;
    let request = match frame.message {
        Some(adapter_frame::Message::ProjectAnalysisRequest(request)) => request,
        _ => return Err(AdapterHostError::UnexpectedFrame),
    };
    cancellation.check()?;
    let result = handler(request.clone(), cancellation)?;
    cancellation.check()?;
    validate_handler_result(&request, &result)?;
    write_project_result(writer, result)
}

fn write_project_result(
    writer: &mut impl Write,
    result: ProjectAnalysisResult,
) -> Result<(), AdapterHostError> {
    let fits_single_frame = result.normalized_ir.len() <= RESULT_CHUNK_BYTES;
    let mut frame = AdapterFrame {
        message: Some(adapter_frame::Message::ProjectAnalysisResult(result)),
    };
    if fits_single_frame {
        let packet = encode_length_delimited_adapter_frame(&frame)?;
        writer
            .write_all(&packet)
            .and_then(|()| writer.flush())
            .map_err(|_| AdapterHostError::ProcessIo)?;
        return Ok(());
    }
    let Some(adapter_frame::Message::ProjectAnalysisResult(result)) = frame.message.take() else {
        return Err(AdapterHostError::UnexpectedFrame);
    };

    let total_output_bytes =
        u64::try_from(result.normalized_ir.len()).map_err(|_| AdapterHostError::Limit)?;
    let chunk_count = result.normalized_ir.len().div_ceil(RESULT_CHUNK_BYTES);
    let chunk_count = u32::try_from(chunk_count).map_err(|_| AdapterHostError::Limit)?;
    if chunk_count < 2 {
        return Err(AdapterHostError::ProjectOutputLimit);
    }
    for (chunk_index, normalized_ir_chunk) in
        result.normalized_ir.chunks(RESULT_CHUNK_BYTES).enumerate()
    {
        let chunk_digest = content_hash(normalized_ir_chunk);
        let frame = AdapterFrame {
            message: Some(adapter_frame::Message::ProjectAnalysisResultChunk(
                ProjectAnalysisResultChunk {
                    session_id: result.session_id.clone(),
                    request_id: result.request_id.clone(),
                    chunk_index: u32::try_from(chunk_index).map_err(|_| AdapterHostError::Limit)?,
                    normalized_ir_chunk: normalized_ir_chunk.to_vec(),
                    chunk_digest: Some(rootlight_protocol::generated::common::v1::ContentHash {
                        value: chunk_digest.as_bytes().to_vec(),
                    }),
                },
            )),
        };
        let packet = encode_length_delimited_adapter_frame(&frame)?;
        writer
            .write_all(&packet)
            .map_err(|_| AdapterHostError::ProcessIo)?;
    }
    let end = AdapterFrame {
        message: Some(adapter_frame::Message::ProjectAnalysisResultEnd(
            ProjectAnalysisResultEnd {
                session_id: result.session_id,
                request_id: result.request_id,
                chunk_count,
                total_output_bytes,
                output_digest: result.output_digest,
            },
        )),
    };
    let packet = encode_length_delimited_adapter_frame(&end)?;
    writer
        .write_all(&packet)
        .map_err(|_| AdapterHostError::ProcessIo)?;
    writer.flush().map_err(|_| AdapterHostError::ProcessIo)
}

fn read_exact_frame(
    reader: &mut impl Read,
    cancellation: &Cancellation,
) -> Result<AdapterFrame, AdapterHostError> {
    let mut decoder = AdapterFrameDecoder::new();
    let mut buffer = [0_u8; PIPE_CHUNK_BYTES];
    loop {
        cancellation.check()?;
        let bytes = reader
            .read(&mut buffer)
            .map_err(|_| AdapterHostError::ProcessIo)?;
        if bytes == 0 {
            return Err(AdapterHostError::UnexpectedFrame);
        }
        let (consumed, frame) = decoder.push(&buffer[..bytes])?;
        if consumed != bytes {
            return Err(AdapterHostError::UnexpectedFrame);
        }
        if let Some(frame) = frame {
            cancellation.check()?;
            let mut trailing = [0_u8; 1];
            if reader
                .read(&mut trailing)
                .map_err(|_| AdapterHostError::ProcessIo)?
                != 0
            {
                return Err(AdapterHostError::UnexpectedFrame);
            }
            return Ok(frame);
        }
    }
}

fn validate_handler_result(
    request: &ProjectAnalysisRequest,
    result: &ProjectAnalysisResult,
) -> Result<(), AdapterHostError> {
    if result.session_id != request.session_id || result.request_id != request.request_id {
        return Err(AdapterHostError::RequestMismatch);
    }
    if result.normalized_ir.is_empty()
        || result.normalized_ir.len() > MAX_ADAPTER_PROJECT_OUTPUT_BYTES
        || result.output_digest.as_ref().is_none_or(|digest| {
            digest.value.as_slice() != content_hash(&result.normalized_ir).as_bytes()
        })
    {
        return Err(AdapterHostError::DigestMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use rootlight_protocol::{
        adapter_contract::{AdapterFrameDecoder, encode_length_delimited_adapter_frame},
        generated::{
            adapter::v1::{
                AdapterFrame, ProjectAnalysisRequest, ProjectAnalysisResult, adapter_frame,
            },
            common::v1::ContentHash,
        },
    };

    use super::*;

    #[test]
    fn one_request_produces_one_correlated_authenticated_result() {
        let request = request();
        let input = encode_length_delimited_adapter_frame(&AdapterFrame {
            message: Some(adapter_frame::Message::ProjectAnalysisRequest(
                request.clone(),
            )),
        })
        .expect("request frame encodes");
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        serve_project_session(
            &mut reader,
            &mut output,
            &Cancellation::new(),
            |observed, _| {
                assert_eq!(observed, request);
                Ok(result(&observed))
            },
        )
        .expect("one session is served");

        let mut decoder = AdapterFrameDecoder::new();
        let (consumed, frame) = decoder.push(&output).expect("response decodes");
        assert_eq!(consumed, output.len());
        assert!(matches!(
            frame.and_then(|frame| frame.message),
            Some(adapter_frame::Message::ProjectAnalysisResult(_))
        ));
    }

    #[test]
    fn oversized_project_result_is_emitted_as_ordered_authenticated_chunks() {
        let request = request();
        let input = encode_length_delimited_adapter_frame(&AdapterFrame {
            message: Some(adapter_frame::Message::ProjectAnalysisRequest(
                request.clone(),
            )),
        })
        .expect("request frame encodes");
        let normalized_ir = vec![7; RESULT_CHUNK_BYTES + 1];
        let expected_digest = content_hash(&normalized_ir);
        let mut output = Vec::new();

        serve_project_session(
            &mut Cursor::new(input),
            &mut output,
            &Cancellation::new(),
            |observed, _| {
                Ok(ProjectAnalysisResult {
                    session_id: observed.session_id,
                    request_id: observed.request_id,
                    output_digest: Some(ContentHash {
                        value: expected_digest.as_bytes().to_vec(),
                    }),
                    normalized_ir,
                })
            },
        )
        .expect("chunked session is served");

        let mut decoder = AdapterFrameDecoder::new();
        let mut offset = 0usize;
        let mut chunks = Vec::new();
        while offset < output.len() {
            let (consumed, frame) = decoder.push(&output[offset..]).expect("chunk decodes");
            offset += consumed;
            if let Some(AdapterFrame {
                message: Some(adapter_frame::Message::ProjectAnalysisResultChunk(chunk)),
            }) = frame
            {
                chunks.push(chunk);
            }
        }
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[1].chunk_index, 1);
        assert!(chunks.iter().all(|chunk| {
            chunk.chunk_digest.as_ref().is_some_and(|digest| {
                digest.value.as_slice() == content_hash(&chunk.normalized_ir_chunk).as_bytes()
            })
        }));
        let reassembled: Vec<_> = chunks
            .iter()
            .flat_map(|chunk| chunk.normalized_ir_chunk.iter().copied())
            .collect();
        assert_eq!(reassembled.len(), RESULT_CHUNK_BYTES + 1);
        assert_eq!(content_hash(&reassembled), expected_digest);
        let mut decoder = AdapterFrameDecoder::new();
        let mut offset = 0usize;
        let mut end = None;
        while offset < output.len() {
            let (consumed, frame) = decoder.push(&output[offset..]).expect("frame decodes");
            offset += consumed;
            if let Some(AdapterFrame {
                message: Some(adapter_frame::Message::ProjectAnalysisResultEnd(observed)),
            }) = frame
            {
                end = Some(observed);
            }
        }
        let end = end.expect("chunk stream has an authenticated commit marker");
        assert_eq!(end.chunk_count, 2);
        assert_eq!(
            end.total_output_bytes,
            u64::try_from(RESULT_CHUNK_BYTES + 1).expect("fixture size is representable")
        );
        assert_eq!(
            end.output_digest.expect("result digest exists").value,
            expected_digest.as_bytes()
        );
    }

    #[test]
    fn trailing_input_and_handler_substitution_emit_no_partial_output() {
        let request = request();
        let mut input = encode_length_delimited_adapter_frame(&AdapterFrame {
            message: Some(adapter_frame::Message::ProjectAnalysisRequest(
                request.clone(),
            )),
        })
        .expect("request frame encodes");
        input.push(0);
        let mut output = Vec::new();
        assert!(matches!(
            serve_project_session(
                &mut Cursor::new(input),
                &mut output,
                &Cancellation::new(),
                |observed, _| Ok(result(&observed)),
            ),
            Err(AdapterHostError::UnexpectedFrame)
        ));
        assert!(output.is_empty());

        let exact = encode_length_delimited_adapter_frame(&AdapterFrame {
            message: Some(adapter_frame::Message::ProjectAnalysisRequest(
                request.clone(),
            )),
        })
        .expect("request frame encodes");
        assert!(matches!(
            serve_project_session(
                &mut Cursor::new(exact),
                &mut output,
                &Cancellation::new(),
                |observed, _| {
                    let mut result = result(&observed);
                    result.request_id.fill(9);
                    Ok(result)
                },
            ),
            Err(AdapterHostError::RequestMismatch)
        ));
        assert!(output.is_empty());
    }

    fn request() -> ProjectAnalysisRequest {
        ProjectAnalysisRequest {
            session_id: vec![1; 16],
            request_id: vec![2; 16],
            repository: None,
            generation: None,
            analysis_unit: "workspace".to_owned(),
            target: "default".to_owned(),
            build_context: None,
            config_digest: None,
            inputs: Vec::new(),
            context_manifest: b"{}".to_vec(),
            requested_tier: 1,
        }
    }

    fn result(request: &ProjectAnalysisRequest) -> ProjectAnalysisResult {
        let normalized_ir = b"normalized".to_vec();
        ProjectAnalysisResult {
            session_id: request.session_id.clone(),
            request_id: request.request_id.clone(),
            output_digest: Some(ContentHash {
                value: content_hash(&normalized_ir).as_bytes().to_vec(),
            }),
            normalized_ir,
        }
    }
}
