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
        AdapterFrameDecoder, MAX_ADAPTER_FRAME_BYTES, encode_length_delimited_adapter_frame,
    },
    generated::adapter::v1::{
        AdapterFrame, ProjectAnalysisRequest, ProjectAnalysisResult, adapter_frame,
    },
};

use crate::AdapterHostError;

const PIPE_CHUNK_BYTES: usize = 16 * 1024;

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
    let packet = encode_length_delimited_adapter_frame(&AdapterFrame {
        message: Some(adapter_frame::Message::ProjectAnalysisResult(result)),
    })?;
    writer
        .write_all(&packet)
        .and_then(|()| writer.flush())
        .map_err(|_| AdapterHostError::ProcessIo)
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
        || result.normalized_ir.len() > MAX_ADAPTER_FRAME_BYTES
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
