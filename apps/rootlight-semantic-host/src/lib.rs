//! Bounded stdio protocol for the opt-in local semantic process.
//!
//! The host accepts source-free vectors and artifact bytes only. It has no
//! path-bearing operation, model runtime, network client, or implicit
//! persistence path.

#![forbid(unsafe_code)]

use std::io::{self, BufRead, Write};

use data_encoding::BASE64URL_NOPAD;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{ContentHash, GenerationId, RepositoryId};
use rootlight_semantic::{
    BuiltSemanticArtifact, SemanticAccounting, SemanticContext, SemanticError, SemanticItem,
    SemanticLimits, SemanticMatch, SemanticQuery, build_artifact, query_artifact,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Version of the semantic host request/response protocol.
pub const SEMANTIC_HOST_PROTOCOL: &str = "rootlight.semantic-host/1";

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_REQUEST_ID_BYTES: usize = 128;

/// Fatal host transport failures.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SemanticHostError {
    /// Stdio failed while receiving a frame.
    #[error("semantic host input failed")]
    Input(#[source] io::Error),
    /// Stdio failed while emitting a response.
    #[error("semantic host output failed")]
    Output(#[source] io::Error),
    /// A frame exceeded the process-wide hard byte ceiling.
    #[error("semantic host frame limit exceeded")]
    FrameLimitExceeded,
    /// An internal response could not be represented within the protocol.
    #[error("semantic host response encoding failed")]
    ResponseEncoding,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostRequest {
    schema: String,
    request_id: String,
    operation: HostOperation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
enum HostOperation {
    Health(HealthRequest),
    Build(BuildRequest),
    Query(QueryRequest),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthRequest {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildRequest {
    repository: RepositoryId,
    generation: GenerationId,
    model_id: String,
    model_hash: ContentHash,
    chunk_policy_version: String,
    items: Vec<WireItem>,
    limits: WireLimits,
    cancelled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryRequest {
    artifact_base64: String,
    repository: RepositoryId,
    generation: GenerationId,
    model_id: String,
    model_hash: ContentHash,
    chunk_policy_version: String,
    vector: Vec<f32>,
    max_results: usize,
    limits: WireLimits,
    cancelled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireItem {
    item_id: String,
    content_hash: ContentHash,
    vector: Vec<f32>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireLimits {
    max_input_bytes: usize,
    max_disk_bytes: usize,
    max_memory_bytes: usize,
    max_items: usize,
    max_dimensions: usize,
    max_results: usize,
}

impl WireLimits {
    fn validate(self) -> Result<SemanticLimits, SemanticError> {
        SemanticLimits::default()
            .with_max_input_bytes(self.max_input_bytes)?
            .with_max_disk_bytes(self.max_disk_bytes)?
            .with_max_memory_bytes(self.max_memory_bytes)?
            .with_max_items(self.max_items)?
            .with_max_dimensions(self.max_dimensions)?
            .with_max_results(self.max_results)
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct HostResponse {
    schema: &'static str,
    request_id: String,
    outcome: ResponseOutcome,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
enum ResponseOutcome {
    Ok(HostResult),
    Error(ErrorResponse),
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
enum HostResult {
    Health(HealthResponse),
    Build(BuildResponse),
    Query(QueryResponse),
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct HealthResponse {
    capability: &'static str,
    persistence: &'static str,
    network: &'static str,
    repository_filesystem: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct BuildResponse {
    repository: RepositoryId,
    generation: GenerationId,
    artifact_base64: String,
    accounting: WireAccounting,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct QueryResponse {
    repository: RepositoryId,
    generation: GenerationId,
    matches: Vec<SemanticMatch>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ErrorResponse {
    code: HostErrorCode,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum HostErrorCode {
    InvalidSchema,
    InvalidRequestId,
    MalformedRequest,
    InvalidLimits,
    InvalidIdentifier,
    InvalidVector,
    InputLimitExceeded,
    ItemLimitExceeded,
    DimensionMismatch,
    ResultLimitExceeded,
    MemoryLimitExceeded,
    DiskLimitExceeded,
    DuplicateItem,
    RepositoryMismatch,
    GenerationMismatch,
    ModelMismatch,
    ChunkPolicyMismatch,
    MalformedArtifact,
    NonCanonicalArtifact,
    IntegrityMismatch,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
struct WireAccounting {
    input_bytes: usize,
    memory_bytes: usize,
    disk_bytes: usize,
    items: usize,
    dimensions: usize,
}

impl From<SemanticAccounting> for WireAccounting {
    fn from(value: SemanticAccounting) -> Self {
        Self {
            input_bytes: value.input_bytes(),
            memory_bytes: value.memory_bytes(),
            disk_bytes: value.disk_bytes(),
            items: value.items(),
            dimensions: value.dimensions(),
        }
    }
}

impl From<SemanticError> for HostErrorCode {
    fn from(value: SemanticError) -> Self {
        match value {
            SemanticError::InvalidLimits => Self::InvalidLimits,
            SemanticError::InvalidIdentifier => Self::InvalidIdentifier,
            SemanticError::InvalidVector => Self::InvalidVector,
            SemanticError::InputLimitExceeded => Self::InputLimitExceeded,
            SemanticError::ItemLimitExceeded => Self::ItemLimitExceeded,
            SemanticError::DimensionMismatch => Self::DimensionMismatch,
            SemanticError::ResultLimitExceeded => Self::ResultLimitExceeded,
            SemanticError::MemoryLimitExceeded => Self::MemoryLimitExceeded,
            SemanticError::DiskLimitExceeded => Self::DiskLimitExceeded,
            SemanticError::DuplicateItem => Self::DuplicateItem,
            SemanticError::RepositoryMismatch => Self::RepositoryMismatch,
            SemanticError::GenerationMismatch => Self::GenerationMismatch,
            SemanticError::ModelMismatch => Self::ModelMismatch,
            SemanticError::ChunkPolicyMismatch => Self::ChunkPolicyMismatch,
            SemanticError::MalformedArtifact => Self::MalformedArtifact,
            SemanticError::NonCanonicalArtifact => Self::NonCanonicalArtifact,
            SemanticError::IntegrityMismatch => Self::IntegrityMismatch,
            SemanticError::Cancelled => Self::Cancelled,
            _ => Self::MalformedRequest,
        }
    }
}

/// Serves versioned semantic requests until stdin reaches EOF.
///
/// Malformed requests receive a source-free error response. An oversized frame
/// terminates the process because no request identifier can be trusted beyond
/// the frame boundary.
///
/// # Errors
///
/// Returns [`SemanticHostError`] for fatal stdio, frame-size, or response
/// serialization failures.
pub fn serve_stdio() -> Result<(), SemanticHostError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve(stdin.lock(), stdout.lock())
}

/// Serves the semantic protocol over caller-provided buffered streams.
///
/// # Errors
///
/// Returns [`SemanticHostError`] for fatal transport, size, or serialization
/// failures. Domain failures are encoded as normal protocol responses.
pub fn serve<R: BufRead, W: Write>(mut reader: R, mut writer: W) -> Result<(), SemanticHostError> {
    while let Some(frame) = read_bounded_frame(&mut reader)? {
        let response = handle_frame(&frame);
        write_response(&mut writer, &response)?;
    }
    Ok(())
}

fn handle_frame(frame: &[u8]) -> HostResponse {
    let parsed = serde_json::from_slice::<HostRequest>(frame);
    let Ok(request) = parsed else {
        return error_response(String::new(), HostErrorCode::MalformedRequest);
    };
    if request.schema != SEMANTIC_HOST_PROTOCOL {
        return error_response(request.request_id, HostErrorCode::InvalidSchema);
    }
    if !valid_request_id(&request.request_id) {
        return error_response(String::new(), HostErrorCode::InvalidRequestId);
    }
    let request_id = request.request_id;
    let outcome = match request.operation {
        HostOperation::Health(_) => ResponseOutcome::Ok(HostResult::Health(HealthResponse {
            capability: "explicit_local_vectors_only",
            persistence: "none",
            network: "unavailable",
            repository_filesystem: "unavailable",
        })),
        HostOperation::Build(build) => execute_build(build),
        HostOperation::Query(query) => execute_query(query),
    };
    HostResponse {
        schema: SEMANTIC_HOST_PROTOCOL,
        request_id,
        outcome,
    }
}

fn execute_build(request: BuildRequest) -> ResponseOutcome {
    let result = (|| {
        let limits = request.limits.validate()?;
        let cancellation = request_cancellation(request.cancelled);
        cancellation.check().map_err(|_| SemanticError::Cancelled)?;
        let context = SemanticContext::new(
            request.repository,
            request.generation,
            request.model_id,
            request.model_hash,
            request.chunk_policy_version,
        )?;
        let items = request
            .items
            .into_iter()
            .map(|item| SemanticItem::new(item.item_id, item.content_hash, item.vector))
            .collect::<Result<Vec<_>, _>>()?;
        build_artifact(context, items, limits, &cancellation)
    })();
    match result {
        Ok(artifact) => build_success(artifact),
        Err(error) => ResponseOutcome::Error(ErrorResponse { code: error.into() }),
    }
}

fn build_success(artifact: BuiltSemanticArtifact) -> ResponseOutcome {
    let repository = artifact.repository();
    let generation = artifact.generation();
    let accounting = artifact.accounting().into();
    let artifact_base64 = BASE64URL_NOPAD.encode(artifact.as_bytes());
    ResponseOutcome::Ok(HostResult::Build(BuildResponse {
        repository,
        generation,
        artifact_base64,
        accounting,
    }))
}

fn execute_query(request: QueryRequest) -> ResponseOutcome {
    let result = (|| {
        let limits = request.limits.validate()?;
        let cancellation = request_cancellation(request.cancelled);
        cancellation.check().map_err(|_| SemanticError::Cancelled)?;
        let artifact = BASE64URL_NOPAD
            .decode(request.artifact_base64.as_bytes())
            .map_err(|_| SemanticError::MalformedArtifact)?;
        if artifact.len() > limits.max_disk_bytes() {
            return Err(SemanticError::DiskLimitExceeded);
        }
        let context = SemanticContext::new(
            request.repository,
            request.generation,
            request.model_id,
            request.model_hash,
            request.chunk_policy_version,
        )?;
        let query = SemanticQuery::new(context, request.vector, request.max_results)?;
        query_artifact(&artifact, &query, limits, &cancellation)
    })();
    match result {
        Ok(response) => {
            let repository = response.repository();
            let generation = response.generation();
            ResponseOutcome::Ok(HostResult::Query(QueryResponse {
                repository,
                generation,
                matches: response.into_matches(),
            }))
        }
        Err(error) => ResponseOutcome::Error(ErrorResponse { code: error.into() }),
    }
}

fn request_cancellation(cancelled: bool) -> Cancellation {
    let cancellation = Cancellation::new();
    if cancelled {
        let _ = cancellation.cancel(CancellationReason::ClientRequest);
    }
    cancellation
}

fn error_response(request_id: String, code: HostErrorCode) -> HostResponse {
    HostResponse {
        schema: SEMANTIC_HOST_PROTOCOL,
        request_id,
        outcome: ResponseOutcome::Error(ErrorResponse { code }),
    }
}

fn read_bounded_frame<R: BufRead>(reader: &mut R) -> Result<Option<Vec<u8>>, SemanticHostError> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(SemanticHostError::Input)?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Ok(Some(frame))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |position| position);
        let next_len = frame
            .len()
            .checked_add(take)
            .ok_or(SemanticHostError::FrameLimitExceeded)?;
        if next_len > MAX_FRAME_BYTES {
            return Err(SemanticHostError::FrameLimitExceeded);
        }
        frame.extend_from_slice(
            available
                .get(..take)
                .ok_or(SemanticHostError::FrameLimitExceeded)?,
        );
        let consumed = take + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return Ok(Some(frame));
        }
    }
}

fn write_response<W: Write>(
    writer: &mut W,
    response: &HostResponse,
) -> Result<(), SemanticHostError> {
    let encoded = serde_json::to_vec(response).map_err(|_| SemanticHostError::ResponseEncoding)?;
    if encoded.len() > MAX_FRAME_BYTES {
        return Err(SemanticHostError::FrameLimitExceeded);
    }
    writer
        .write_all(&encoded)
        .and_then(|()| writer.write_all(b"\n"))
        .and_then(|()| writer.flush())
        .map_err(SemanticHostError::Output)
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::Value;

    use super::{MAX_FRAME_BYTES, SemanticHostError, serve};

    #[test]
    fn hostile_json_and_unknown_fields_fail_closed() {
        for input in [
            b"{not-json}\n".as_slice(),
            br#"{"schema":"rootlight.semantic-host/1","request_id":"a","operation":{"kind":"health","payload":{"unknown":true}}}
"#,
            br#"{"schema":"rootlight.semantic-host/1","request_id":"a","operation":{"kind":"health","payload":{},"unknown":true}}
"#,
            br#"{"schema":"rootlight.semantic-host/1","request_id":"a","operation":{"kind":"unknown","payload":{}}}
"#,
        ] {
            let mut output = Vec::new();
            serve(Cursor::new(input), &mut output).expect("malformed input gets a response");
            let response: Value =
                serde_json::from_slice(&output).expect("response is valid JSON");
            assert_eq!(response["outcome"]["status"], "error");
        }
    }

    #[test]
    fn oversized_frame_fails_without_unbounded_growth() {
        let input = vec![b'a'; MAX_FRAME_BYTES + 1];
        let error = serve(Cursor::new(input), Vec::new()).expect_err("frame must be rejected");
        assert!(matches!(error, SemanticHostError::FrameLimitExceeded));
    }
}
