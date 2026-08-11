//! Production project-session execution for the isolated adapter child.
//!
//! This module converts one hostile protocol request into immutable VFS and
//! SDK types, runs the audited structural analyzer, and returns one correlated
//! normalized-IR transaction without exposing repository paths to the process.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::File,
    io::Read,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisUnitId, BatchThresholds, BuildTargetId, EncodingId,
    GeneratedOriginMapping, GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy,
    ProjectAnalysisLimits, ProjectAnalysisRequest as SdkProjectAnalysisRequest, ProjectSourceInput,
    ReportError, RequestError, ResourceKind, SinkError, StreamLimits, TransformationId,
    execute_project_analysis,
};
use rootlight_adapter_treesitter::{ParserSettings, RuntimeConfig, TreeSitterProvider};
use rootlight_adapters::{SemanticProjectAnalyzer, SemanticProjectLanguage};
use rootlight_cancel::Cancellation;
use rootlight_ids::{
    ContentHash, FileId, FileIdentity, GenerationId, RepositoryId, content_hash, derive_file,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ExtensionSupport, FILE_IDENTITY_CLAIM_NAMESPACE, IrLimits,
    LEXICAL_EXTENSION_NAMESPACE, ProducerIdentity, SYMBOL_IDENTITY_CLAIM_NAMESPACE, SourceRef,
    SourceSpan,
};
use rootlight_protocol::{
    adapter_contract::{
        ADAPTER_DIGEST_BYTES, ADAPTER_NONCE_BYTES, ADAPTER_PROTOCOL_MAJOR,
        CURRENT_ADAPTER_PROTOCOL_MINOR, MAX_ADAPTER_FRAME_BYTES, NegotiatedSession,
        ValidatedAdvertisement,
    },
    generated::{
        adapter::v1::{
            AdapterIdentity, AdapterTrustLevel, CapabilityAdvertisement, GeneratedOrigin,
            ProjectAnalysisRequest, ProjectAnalysisResult, ProjectInput, RequestedAnalysisTier,
            ResourceLimits, SessionRequirements,
        },
        common::v1::{
            ContentHash as WireContentHash, ContractVersion, ExtensionDescriptor, VersionRange,
        },
    },
};
use rootlight_sandbox::{AuthenticatedAdapterExecutable, MAX_ADAPTER_EXECUTABLE_BYTES};
use rootlight_vfs::{RelativePath, SourceSnapshot};

use crate::{AdapterHostError, serve_project_session};

/// Stable producer name advertised for the built-in semantic project adapter.
pub const PROJECT_ADAPTER_NAME: &str = "rootlight-project-semantics";
/// Exact project-adapter host crate version compiled into the executable.
pub const PROJECT_ADAPTER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Stable hard ceiling advertised by the built-in semantic project adapter.
pub const PROJECT_ADAPTER_HARD_LIMITS: ResourceLimits = ResourceLimits {
    wall_time_ms: 60_000,
    cpu_time_ms: 45_000,
    memory_bytes: 1024 * 1024 * 1024,
    input_bytes: 16 * 1024 * 1024,
    output_bytes: 128 * 1024 * 1024,
    files: 4_096,
    processes: 1,
    handles: 256,
    retries: 0,
};

pub(crate) const PROJECT_SESSION_ARGUMENT: &str = "--project-session";
pub(crate) const WALL_TIME_ARGUMENT: &str = "--wall-time-ms";
pub(crate) const MEMORY_BYTES_ARGUMENT: &str = "--memory-bytes";
pub(crate) const INPUT_BYTES_ARGUMENT: &str = "--input-bytes";
pub(crate) const OUTPUT_BYTES_ARGUMENT: &str = "--output-bytes";
pub(crate) const FILES_ARGUMENT: &str = "--files";

const MAX_PROJECT_PATH_BYTES: usize = 4_096;
const MAX_PROJECT_CONTEXT_BYTES: usize = 1024 * 1024;
const MAX_PROTOCOL_LABEL_BYTES: usize = 128;
const MAX_SYNTAX_NODES: usize = 16 * 1024 * 1024;
const MAX_SYNTAX_DEPTH: usize = 4_096;
const MAX_INCREMENTAL_CACHE_BYTES: usize = u32::MAX as usize;
const INPUT_CHUNK_BYTES: usize = 64 * 1024;
const IR_BATCH_RECORDS: usize = 256;
const MAX_CANDIDATES_PER_SITE: usize = 256;
const MAX_DIAGNOSTICS: usize = 4_096;

/// Computes the exact executable identity used by protocol advertisement and IR provenance.
///
/// The digest is streamed under a fixed executable-size ceiling. The parent
/// calls this before negotiation, while `--project-session` repeats it over the
/// sandbox's immutable runtime copy.
///
/// # Errors
///
/// Returns [`AdapterHostError::BinaryIdentity`] when the executable cannot be
/// opened, read, or bounded.
pub fn project_adapter_identity(executable: &Path) -> Result<AdapterIdentity, AdapterHostError> {
    let digest = project_adapter_binary_digest(executable)?;
    Ok(project_adapter_identity_from_digest(digest))
}

/// Computes the exact executable identity with cooperative cancellation.
///
/// # Errors
///
/// Returns [`AdapterHostError::Cancelled`] when cancellation wins, or
/// [`AdapterHostError::BinaryIdentity`] when the executable cannot be opened,
/// read, or bounded.
pub fn project_adapter_identity_with_cancellation(
    executable: &Path,
    cancellation: &Cancellation,
) -> Result<AdapterIdentity, AdapterHostError> {
    let digest = project_adapter_binary_digest_with_cancellation(executable, cancellation)?;
    Ok(project_adapter_identity_from_digest(digest))
}

fn project_adapter_identity_from_digest(digest: ContentHash) -> AdapterIdentity {
    AdapterIdentity {
        name: PROJECT_ADAPTER_NAME.to_owned(),
        version: PROJECT_ADAPTER_VERSION.to_owned(),
        source_digest: digest.as_bytes().to_vec(),
    }
}

/// Builds and validates the production advertisement for one exact executable.
///
/// The advertisement fixes protocol 1.3, project normalized IR, first-party
/// lexical evidence, producer-neutral identity claims, first-party trust,
/// cancellation, and the built-in hard resource ceiling.
///
/// # Errors
///
/// Returns a source-free identity or protocol error if the executable cannot
/// be authenticated or the built-in contract no longer validates.
pub fn project_adapter_advertisement(
    executable: &Path,
) -> Result<ValidatedAdvertisement, AdapterHostError> {
    let identity = project_adapter_identity(executable)?;
    project_adapter_advertisement_for_identity(identity)
}

fn project_adapter_advertisement_for_identity(
    identity: AdapterIdentity,
) -> Result<ValidatedAdvertisement, AdapterHostError> {
    let version = project_protocol_version();
    ValidatedAdvertisement::validate(
        CapabilityAdvertisement {
            adapter: Some(identity),
            supported_protocols: Some(VersionRange {
                minimum: Some(version),
                maximum: Some(version),
            }),
            capabilities: vec![crate::PROJECT_NORMALIZED_IR_CAPABILITY.to_owned()],
            extensions: project_extensions(),
            trust_level: AdapterTrustLevel::FirstParty as i32,
            hard_limits: Some(PROJECT_ADAPTER_HARD_LIMITS),
            supports_cancellation: true,
        },
        AdapterTrustLevel::FirstParty,
    )
    .map_err(AdapterHostError::from)
}

/// Negotiates a production session for one exact executable and resource grant.
///
/// `granted_limits` may narrow but never exceed
/// [`PROJECT_ADAPTER_HARD_LIMITS`]. The returned session is ready for
/// [`crate::execute_isolated_project_adapter`].
///
/// # Errors
///
/// Returns a source-free identity or protocol error for an invalid nonce,
/// executable, or resource grant.
pub fn negotiate_project_adapter_session(
    executable: &Path,
    session_id: [u8; ADAPTER_NONCE_BYTES],
    granted_limits: ResourceLimits,
) -> Result<NegotiatedSession, AdapterHostError> {
    let identity = project_adapter_identity(executable)?;
    negotiate_project_adapter_session_for_identity(identity, session_id, granted_limits)
}

/// Authenticates an executable once and negotiates a cancellable production session.
///
/// The returned identity is the exact digest used by the negotiated session, so
/// callers can bind provider provenance without re-reading the executable.
///
/// # Errors
///
/// Returns a source-free cancellation, identity, or protocol error for an
/// invalid nonce, executable, or resource grant.
pub fn negotiate_project_adapter_session_with_cancellation(
    executable: &Path,
    session_id: [u8; ADAPTER_NONCE_BYTES],
    granted_limits: ResourceLimits,
    cancellation: &Cancellation,
) -> Result<(AdapterIdentity, NegotiatedSession), AdapterHostError> {
    let identity = project_adapter_identity_with_cancellation(executable, cancellation)?;
    let session = negotiate_project_adapter_session_for_identity(
        identity.clone(),
        session_id,
        granted_limits,
    )?;
    Ok((identity, session))
}

fn negotiate_project_adapter_session_for_identity(
    identity: AdapterIdentity,
    session_id: [u8; ADAPTER_NONCE_BYTES],
    granted_limits: ResourceLimits,
) -> Result<NegotiatedSession, AdapterHostError> {
    let version = project_protocol_version();
    let advertisement = project_adapter_advertisement_for_identity(identity)?;
    advertisement
        .negotiate(SessionRequirements {
            session_id: session_id.to_vec(),
            selected_protocol: Some(version),
            expected_adapter: Some(advertisement.identity().clone()),
            required_capabilities: vec![crate::PROJECT_NORMALIZED_IR_CAPABILITY.to_owned()],
            required_extensions: project_extensions(),
            granted_limits: Some(granted_limits),
            maximum_trust: AdapterTrustLevel::FirstParty as i32,
            require_cancellation: true,
        })
        .map_err(AdapterHostError::from)
}

/// Runs one production `--project-session` over standard input and output.
///
/// `arguments` must contain the exact source-free limits emitted by
/// [`crate::execute_isolated_project_adapter`]. The process reads no repository
/// path and executes no repository-owned code.
///
/// # Errors
///
/// Returns a source-free [`AdapterHostError`] for malformed limits, binary
/// identity failure, invalid request conversion, analysis failure, or bounded
/// pipe failure. No output is written until the complete result validates.
pub fn run_project_session(
    arguments: impl Iterator<Item = OsString>,
) -> Result<(), AdapterHostError> {
    let (limits, cancellation) = prepare_project_session(arguments)?;
    let executable = std::env::current_exe().map_err(|_| AdapterHostError::BinaryIdentity)?;
    let binary_digest = project_adapter_binary_digest(&executable)?;
    run_project_session_with_digest(limits, &cancellation, binary_digest)
}

/// Runs one production `--project-session` with a staging-authenticated binary identity.
///
/// macOS removes the staged executable from the filesystem after Seatbelt is
/// active. The native launcher therefore supplies the digest computed from the
/// same securely opened source handle that populated the staged executable.
///
/// # Errors
///
/// Returns a source-free [`AdapterHostError`] under the same conditions as
/// [`run_project_session`].
pub fn run_authenticated_project_session(
    arguments: impl Iterator<Item = OsString>,
    executable: AuthenticatedAdapterExecutable,
) -> Result<(), AdapterHostError> {
    let (limits, cancellation) = prepare_project_session(arguments)?;
    run_project_session_with_digest(
        limits,
        &cancellation,
        ContentHash::from_bytes(executable.digest_bytes()),
    )
}

fn prepare_project_session(
    arguments: impl Iterator<Item = OsString>,
) -> Result<(ProjectSessionLimits, Cancellation), AdapterHostError> {
    let limits = ProjectSessionLimits::parse(arguments)?;
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(limits.wall_time_ms))
        .ok_or(AdapterHostError::Limit)?;
    Ok((limits, Cancellation::with_deadline(deadline)))
}

fn run_project_session_with_digest(
    limits: ProjectSessionLimits,
    cancellation: &Cancellation,
    binary_digest: ContentHash,
) -> Result<(), AdapterHostError> {
    let mut reader = std::io::stdin().lock();
    let mut writer = std::io::stdout().lock();
    serve_project_session(
        &mut reader,
        &mut writer,
        cancellation,
        |request, cancellation| {
            analyze_project_request(request, limits, binary_digest, cancellation)
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectSessionLimits {
    wall_time_ms: u64,
    memory_bytes: usize,
    input_bytes: usize,
    output_bytes: usize,
    files: usize,
}

impl ProjectSessionLimits {
    fn parse(mut arguments: impl Iterator<Item = OsString>) -> Result<Self, AdapterHostError> {
        let wall_time_ms = parse_u64_argument(&mut arguments, WALL_TIME_ARGUMENT)?;
        let memory_bytes = parse_u64_argument(&mut arguments, MEMORY_BYTES_ARGUMENT)?;
        let input_bytes = parse_u64_argument(&mut arguments, INPUT_BYTES_ARGUMENT)?;
        let output_bytes = parse_u64_argument(&mut arguments, OUTPUT_BYTES_ARGUMENT)?;
        let files = parse_u64_argument(&mut arguments, FILES_ARGUMENT)?;
        if arguments.next().is_some()
            || wall_time_ms == 0
            || wall_time_ms > PROJECT_ADAPTER_HARD_LIMITS.wall_time_ms
            || memory_bytes == 0
            || memory_bytes > PROJECT_ADAPTER_HARD_LIMITS.memory_bytes
            || input_bytes == 0
            || input_bytes > PROJECT_ADAPTER_HARD_LIMITS.input_bytes
            || output_bytes == 0
            || output_bytes > PROJECT_ADAPTER_HARD_LIMITS.output_bytes
            || files == 0
            || files > u64::from(PROJECT_ADAPTER_HARD_LIMITS.files)
        {
            return Err(AdapterHostError::Limit);
        }
        Ok(Self {
            wall_time_ms,
            memory_bytes: usize::try_from(memory_bytes).map_err(|_| AdapterHostError::Limit)?,
            input_bytes: usize::try_from(input_bytes).map_err(|_| AdapterHostError::Limit)?,
            output_bytes: usize::try_from(output_bytes).map_err(|_| AdapterHostError::Limit)?,
            files: usize::try_from(files).map_err(|_| AdapterHostError::Limit)?,
        })
    }
}

fn parse_u64_argument(
    arguments: &mut impl Iterator<Item = OsString>,
    expected_name: &str,
) -> Result<u64, AdapterHostError> {
    if arguments.next().as_deref() != Some(OsStr::new(expected_name)) {
        return Err(AdapterHostError::Limit);
    }
    let value = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or(AdapterHostError::Limit)?;
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(AdapterHostError::Limit);
    }
    value.parse().map_err(|_| AdapterHostError::Limit)
}

const fn project_protocol_version() -> ContractVersion {
    ContractVersion {
        major: ADAPTER_PROTOCOL_MAJOR,
        minor: CURRENT_ADAPTER_PROTOCOL_MINOR,
    }
}

fn lexical_extension() -> ExtensionDescriptor {
    project_extension(LEXICAL_EXTENSION_NAMESPACE)
}

fn project_extensions() -> Vec<ExtensionDescriptor> {
    vec![
        project_extension(FILE_IDENTITY_CLAIM_NAMESPACE),
        project_extension(SYMBOL_IDENTITY_CLAIM_NAMESPACE),
        lexical_extension(),
    ]
}

fn project_extension(namespace: &str) -> ExtensionDescriptor {
    ExtensionDescriptor {
        namespace: namespace.to_owned(),
        version: Some(ContractVersion { major: 1, minor: 0 }),
        critical: false,
    }
}

fn analyze_project_request(
    request: ProjectAnalysisRequest,
    limits: ProjectSessionLimits,
    binary_digest: ContentHash,
    cancellation: &Cancellation,
) -> Result<ProjectAnalysisResult, AdapterHostError> {
    cancellation.check()?;
    validate_nonce(&request.session_id)?;
    validate_nonce(&request.request_id)?;
    validate_metadata_label(&request.analysis_unit)?;
    validate_metadata_label(&request.target)?;
    if request.inputs.is_empty() {
        return Err(AdapterHostError::ProjectRequest);
    }
    if request.inputs.len() > limits.files {
        return Err(AdapterHostError::ProjectInputLimit);
    }

    let repository = RepositoryId::from_bytes(fixed_wire_id(
        request
            .repository
            .as_ref()
            .map(|value| value.value.as_slice()),
    )?);
    let generation = GenerationId::from_bytes(fixed_wire_id(
        request
            .generation
            .as_ref()
            .map(|value| value.value.as_slice()),
    )?);
    let build_context_digest = wire_digest(request.build_context.as_ref())?;
    let configuration_digest = wire_digest(request.config_digest.as_ref())?;
    if request.context_manifest.is_empty()
        || content_hash(&request.context_manifest) != configuration_digest
    {
        return Err(AdapterHostError::ProjectRequest);
    }
    if request.context_manifest.len() > MAX_PROJECT_CONTEXT_BYTES
        || request.context_manifest.len() > limits.input_bytes
    {
        return Err(AdapterHostError::ProjectInputLimit);
    }
    let requested_tier = requested_tier(request.requested_tier)?;
    validate_wire_input_order(&request.inputs)?;

    let mut total_input_bytes = request.context_manifest.len();
    let mut decoded_inputs = Vec::new();
    let mut selected_language = None;
    for input in request.inputs.iter().cloned() {
        cancellation.check()?;
        total_input_bytes = total_input_bytes
            .checked_add(input.source.len())
            .ok_or(AdapterHostError::Limit)?;
        if total_input_bytes > limits.input_bytes {
            return Err(AdapterHostError::ProjectInputLimit);
        }
        let language = semantic_language(&input.language)?;
        if selected_language.is_some_and(|selected| selected != language) {
            return Err(AdapterHostError::ProjectRequest);
        }
        selected_language = Some(language);
        decoded_inputs.push(decode_input(repository, input)?);
    }
    let language = selected_language.ok_or(AdapterHostError::ProjectRequest)?;
    decoded_inputs.sort_by(|left, right| {
        left.snapshot
            .path()
            .identity_bytes()
            .cmp(right.snapshot.path().identity_bytes())
    });
    validate_input_order(&decoded_inputs)?;

    let runtime_config = parser_config(&decoded_inputs, limits)?;
    let parser =
        TreeSitterProvider::new(runtime_config).map_err(|_| AdapterHostError::ProjectAnalysis)?;
    let producer = ProducerIdentity::new(
        PROJECT_ADAPTER_NAME,
        PROJECT_ADAPTER_VERSION,
        configuration_digest,
    )
    .map_err(|_| AdapterHostError::ProjectAnalysis)?;
    let build_context = BuildContextIdentity::new(build_context_digest);
    let analyzer = SemanticProjectAnalyzer::new(
        language,
        Arc::new(parser),
        producer,
        binary_digest,
        build_context,
    )
    .map_err(|_| AdapterHostError::ProjectAnalysis)?;

    let analysis_limits = analysis_limits(&request, &decoded_inputs, limits)?;
    let source_refs = decoded_inputs
        .iter()
        .map(|input| {
            let end = u64::try_from(input.snapshot.content().len())
                .map_err(|_| AdapterHostError::Limit)?;
            let span = SourceSpan::new(input.snapshot.file(), 0, end)
                .map_err(|_| AdapterHostError::ProjectRequest)?;
            Ok(SourceRef::new(
                repository,
                generation,
                span,
                input.snapshot.content_hash(),
                None,
            ))
        })
        .collect::<Result<Vec<_>, AdapterHostError>>()?;
    let path_files = decoded_inputs
        .iter()
        .map(|input| {
            (
                input.snapshot.path().identity_bytes().to_vec(),
                input.snapshot.file(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let sdk_inputs = decoded_inputs
        .iter()
        .zip(&source_refs)
        .map(|(input, source)| {
            let origins = input
                .origins
                .iter()
                .map(|origin| decode_origin(repository, input.snapshot.file(), origin, &path_files))
                .collect::<Result<Vec<_>, _>>()?;
            let snapshot = GenerationBoundSnapshot::new(&input.snapshot, source)
                .map_err(|_| AdapterHostError::ProjectRequest)?;
            Ok(ProjectSourceInput::new(
                snapshot,
                LanguageId::new(language.as_str()).map_err(|_| AdapterHostError::ProjectRequest)?,
                EncodingId::utf8(),
                input.generated,
                origins,
            ))
        })
        .collect::<Result<Vec<_>, AdapterHostError>>()?;
    let sdk_request = SdkProjectAnalysisRequest::new(
        AnalysisUnitId::new(&request.analysis_unit)
            .map_err(|_| AdapterHostError::ProjectRequest)?,
        BuildTargetId::new(&request.target).map_err(|_| AdapterHostError::ProjectRequest)?,
        build_context,
        configuration_digest,
        &request.context_manifest,
        sdk_inputs,
        requested_tier,
        &analysis_limits,
    )
    .map_err(|_| AdapterHostError::ProjectRequest)?;

    // The SDK cannot observe the enclosing Job Object. Admission is allowed
    // here only because this entry point is launched by the hard sandbox path.
    let output = execute_project_analysis(
        &analyzer,
        &sdk_request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        cancellation,
    )
    .map_err(map_project_execution_error)?;
    let normalized_ir =
        serde_json::to_vec(output.document()).map_err(|_| AdapterHostError::ProjectAnalysis)?;
    if normalized_ir.is_empty() {
        return Err(AdapterHostError::ProjectAnalysis);
    }
    if normalized_ir.len() > limits.output_bytes {
        return Err(AdapterHostError::ProjectOutputLimit);
    }
    let output_digest = content_hash(&normalized_ir);
    Ok(ProjectAnalysisResult {
        session_id: request.session_id,
        request_id: request.request_id,
        normalized_ir,
        output_digest: Some(WireContentHash {
            value: output_digest.as_bytes().to_vec(),
        }),
    })
}

fn map_project_execution_error(error: AdapterError) -> AdapterHostError {
    match error {
        AdapterError::RejectedRequest(RequestError::ProviderLimit { resource, .. })
        | AdapterError::Sink(SinkError::BatchLimit { resource, .. })
        | AdapterError::Sink(SinkError::StreamLimit { resource, .. })
        | AdapterError::InvalidReport(ReportError::ResourceLimit { resource, .. }) => {
            project_resource_limit_error(resource)
        }
        AdapterError::Sink(SinkError::AllocationFailed) => AdapterHostError::ProjectMemoryLimit,
        _ => AdapterHostError::ProjectAnalysis,
    }
}

fn project_resource_limit_error(resource: ResourceKind) -> AdapterHostError {
    match resource {
        ResourceKind::SourceBytes
        | ResourceKind::ProjectFiles
        | ResourceKind::ProjectSourceBytes
        | ResourceKind::ProjectContextBytes
        | ResourceKind::GeneratedMappings
        | ResourceKind::GeneratedMappingBytes
        | ResourceKind::AnalysisUnitBytes
        | ResourceKind::BuildTargetBytes
        | ResourceKind::IncludedRanges
        | ResourceKind::SyntaxNodes
        | ResourceKind::SyntaxDepth => AdapterHostError::ProjectInputLimit,
        ResourceKind::ReportedMemoryBytes => AdapterHostError::ProjectMemoryLimit,
        _ => AdapterHostError::ProjectOutputLimit,
    }
}

struct DecodedProjectInput {
    snapshot: SourceSnapshot,
    generated: bool,
    origins: Vec<GeneratedOrigin>,
}

fn decode_input(
    repository: RepositoryId,
    input: ProjectInput,
) -> Result<DecodedProjectInput, AdapterHostError> {
    let path = protocol_path(&input.path)?;
    let file = FileId::from_bytes(fixed_wire_id(
        input.file.as_ref().map(|value| value.value.as_slice()),
    )?);
    let source_digest = wire_digest(input.source_digest.as_ref())?;
    if content_hash(&input.source) != source_digest {
        return Err(AdapterHostError::ProjectRequest);
    }
    validate_origins(&input.origins, input.source.len())?;
    if !input.generated && !input.origins.is_empty() {
        return Err(AdapterHostError::ProjectRequest);
    }
    let snapshot =
        SourceSnapshot::from_persisted(repository, path, file, source_digest, input.source)
            .map_err(|_| AdapterHostError::ProjectRequest)?;
    Ok(DecodedProjectInput {
        snapshot,
        generated: input.generated,
        origins: input.origins,
    })
}

fn decode_origin(
    repository: RepositoryId,
    generated_file: FileId,
    origin: &GeneratedOrigin,
    path_files: &BTreeMap<Vec<u8>, FileId>,
) -> Result<GeneratedOriginMapping, AdapterHostError> {
    let origin_path = protocol_path(&origin.origin_path)?;
    let origin_file = path_files
        .get(origin_path.identity_bytes())
        .copied()
        .unwrap_or_else(|| {
            derive_file(FileIdentity {
                repository,
                path_identity: origin_path.identity_bytes(),
            })
            .id()
        });
    let generated = SourceSpan::new(
        generated_file,
        origin.generated_start_byte,
        origin.generated_end_byte,
    )
    .map_err(|_| AdapterHostError::ProjectRequest)?;
    let source = SourceSpan::new(
        origin_file,
        origin.origin_start_byte,
        origin.origin_end_byte,
    )
    .map_err(|_| AdapterHostError::ProjectRequest)?;
    let transformation = TransformationId::new(&origin.transformation)
        .map_err(|_| AdapterHostError::ProjectRequest)?;
    let generator_digest = origin
        .generator_digest
        .as_ref()
        .map(|digest| wire_digest(Some(digest)))
        .transpose()?;
    Ok(GeneratedOriginMapping::new(
        generated,
        origin_path,
        source,
        transformation,
        generator_digest,
    ))
}

fn analysis_limits(
    request: &ProjectAnalysisRequest,
    inputs: &[DecodedProjectInput],
    session: ProjectSessionLimits,
) -> Result<AnalysisLimits, AdapterHostError> {
    let max_source_bytes = inputs
        .iter()
        .map(|input| input.snapshot.content().len())
        .max()
        .unwrap_or(0)
        .max(1);
    let max_syntax_nodes =
        project_syntax_node_limit(inputs.iter().map(|input| input.snapshot.content().len()))?;
    let max_syntax_depth = max_source_bytes
        .saturating_add(1)
        .clamp(1, MAX_SYNTAX_DEPTH);
    let max_records = session.output_bytes.max(1);
    let max_diagnostics = max_records.clamp(1, MAX_DIAGNOSTICS);
    let batch_records = max_records.clamp(1, IR_BATCH_RECORDS);
    let batch = BatchThresholds::new(
        batch_records,
        session.output_bytes,
        max_diagnostics,
        session.output_bytes,
    )
    .map_err(|_| AdapterHostError::Limit)?;
    let stream = StreamLimits::new(
        max_records,
        max_records,
        session.output_bytes,
        max_diagnostics,
        session.output_bytes,
        session.output_bytes,
        batch,
    )
    .map_err(|_| AdapterHostError::Limit)?;

    let mut ir = IrLimits::default();
    ir.max_document_bytes = session.output_bytes;
    ir.max_extension_envelope_bytes = session.output_bytes;
    ir.max_files = session.files;
    ir.max_entities = max_records;
    ir.max_occurrences = max_records;
    ir.max_relations = max_records;
    ir.max_provenance_records = max_records;
    ir.max_source_mappings = max_records;
    ir.max_coverage_records = max_records;
    ir.max_skipped_regions = max_records;
    ir.max_diagnostics = max_diagnostics;
    ir.max_extensions = max_records;
    ir.max_total_records = max_records;
    ir.max_nested_items_per_record = max_records.min(MAX_CANDIDATES_PER_SITE);
    ir.max_total_nested_items = max_records;
    ir.max_string_bytes = session.output_bytes;
    ir.max_total_string_bytes = session.output_bytes;
    ir.max_extension_payload_bytes = session.output_bytes;
    ir.max_total_extension_bytes = session.output_bytes;
    ir.max_diagnostic_message_bytes = session.output_bytes.min(4_096);
    ir.max_total_diagnostic_bytes = session.output_bytes;

    let mapping_count = inputs.iter().try_fold(0_usize, |total, input| {
        total
            .checked_add(input.origins.len())
            .ok_or(AdapterHostError::Limit)
    })?;
    let project = ProjectAnalysisLimits::new(
        session.files,
        session.input_bytes,
        session.input_bytes.min(MAX_PROJECT_CONTEXT_BYTES),
        mapping_count,
        if mapping_count == 0 {
            0
        } else {
            session.input_bytes.min(MAX_ADAPTER_FRAME_BYTES)
        },
        request.analysis_unit.len(),
        request.target.len(),
    )
    .map_err(|_| AdapterHostError::Limit)?;
    AnalysisLimits::new(
        max_source_bytes,
        max_syntax_nodes,
        max_syntax_depth,
        0,
        session.memory_bytes,
        stream.clone(),
        stream,
        ir,
    )
    .map(|limits| limits.with_project_limits(project))
    .map_err(|_| AdapterHostError::Limit)
}

fn parser_config(
    inputs: &[DecodedProjectInput],
    session: ProjectSessionLimits,
) -> Result<RuntimeConfig, AdapterHostError> {
    let max_source_bytes = inputs
        .iter()
        .map(|input| input.snapshot.content().len())
        .max()
        .unwrap_or(0)
        .max(1);
    let max_syntax_nodes =
        project_syntax_node_limit(inputs.iter().map(|input| input.snapshot.content().len()))?;
    let max_syntax_depth = max_source_bytes
        .saturating_add(1)
        .clamp(1, MAX_SYNTAX_DEPTH);
    let input_chunk_bytes = max_source_bytes.clamp(1, INPUT_CHUNK_BYTES);
    let cache_from_input = session.input_bytes.saturating_mul(4);
    let cache_from_memory = session.memory_bytes / 4;
    let max_cache_bytes = cache_from_input
        .min(cache_from_memory)
        .clamp(1, MAX_INCREMENTAL_CACHE_BYTES);
    let settings = ParserSettings::new(input_chunk_bytes).map_err(|_| AdapterHostError::Limit)?;
    RuntimeConfig::new(
        max_source_bytes,
        max_syntax_nodes,
        max_syntax_depth,
        1,
        1,
        1,
        max_cache_bytes,
        settings,
    )
    .map_err(|_| AdapterHostError::Limit)
}

fn project_syntax_node_limit(
    source_lengths: impl IntoIterator<Item = usize>,
) -> Result<usize, AdapterHostError> {
    // Project reports aggregate syntax nodes across every input, while source
    // and depth ceilings remain per-file. Every parsed file contributes a root
    // node even when empty, so preserve each input's `2 * bytes + 1` allowance.
    let aggregate = source_lengths
        .into_iter()
        .try_fold(0_usize, |total, bytes| {
            let input = bytes
                .checked_mul(2)
                .and_then(|value| value.checked_add(1))
                .ok_or(AdapterHostError::Limit)?;
            total.checked_add(input).ok_or(AdapterHostError::Limit)
        })?;
    Ok(aggregate.clamp(1, MAX_SYNTAX_NODES))
}

fn validate_nonce(value: &[u8]) -> Result<(), AdapterHostError> {
    if value.len() != ADAPTER_NONCE_BYTES || value.iter().all(|byte| *byte == 0) {
        return Err(AdapterHostError::ProjectRequest);
    }
    Ok(())
}

fn validate_metadata_label(value: &str) -> Result<(), AdapterHostError> {
    if value.is_empty()
        || value.len() > MAX_PROJECT_PATH_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(AdapterHostError::ProjectRequest);
    }
    Ok(())
}

fn validate_protocol_label(value: &str) -> Result<(), AdapterHostError> {
    if value.is_empty()
        || value.len() > MAX_PROTOCOL_LABEL_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return Err(AdapterHostError::ProjectRequest);
    }
    Ok(())
}

fn protocol_path(value: &str) -> Result<RelativePath, AdapterHostError> {
    validate_metadata_label(value)?;
    if value.starts_with('/')
        || value.starts_with('\\')
        || value.contains('\\')
        || value.split('/').any(|component| {
            component.is_empty() || component == "." || component == ".." || component.contains(':')
        })
    {
        return Err(AdapterHostError::ProjectRequest);
    }
    RelativePath::parse(Path::new(value)).map_err(|_| AdapterHostError::ProjectRequest)
}

fn semantic_language(value: &str) -> Result<SemanticProjectLanguage, AdapterHostError> {
    validate_protocol_label(value)?;
    match value {
        "rust" => Ok(SemanticProjectLanguage::Rust),
        "typescript" => Ok(SemanticProjectLanguage::TypeScript),
        "javascript" => Ok(SemanticProjectLanguage::JavaScript),
        "python" => Ok(SemanticProjectLanguage::Python),
        "go" => Ok(SemanticProjectLanguage::Go),
        _ => Err(AdapterHostError::ProjectRequest),
    }
}

fn requested_tier(value: i32) -> Result<AnalysisTier, AdapterHostError> {
    match RequestedAnalysisTier::try_from(value) {
        Ok(RequestedAnalysisTier::TierA) => Ok(AnalysisTier::TierA),
        Ok(RequestedAnalysisTier::TierB) => Ok(AnalysisTier::TierB),
        Ok(
            RequestedAnalysisTier::TierC
            | RequestedAnalysisTier::TierD
            | RequestedAnalysisTier::Unspecified,
        )
        | Err(_) => Err(AdapterHostError::ProjectRequest),
    }
}

fn validate_input_order(inputs: &[DecodedProjectInput]) -> Result<(), AdapterHostError> {
    let mut previous = None::<&[u8]>;
    for input in inputs {
        let current = input.snapshot.path().identity_bytes();
        if previous.is_some_and(|value| value >= current) {
            return Err(AdapterHostError::ProjectRequest);
        }
        previous = Some(current);
    }
    Ok(())
}

fn validate_wire_input_order(inputs: &[ProjectInput]) -> Result<(), AdapterHostError> {
    let mut previous = None::<&str>;
    for input in inputs {
        if previous.is_some_and(|value| value >= input.path.as_str()) {
            return Err(AdapterHostError::ProjectRequest);
        }
        previous = Some(&input.path);
    }
    Ok(())
}

fn validate_origins(
    origins: &[GeneratedOrigin],
    source_bytes: usize,
) -> Result<(), AdapterHostError> {
    let source_bytes = u64::try_from(source_bytes).map_err(|_| AdapterHostError::Limit)?;
    let mut previous_end = 0_u64;
    for (index, origin) in origins.iter().enumerate() {
        let _ = protocol_path(&origin.origin_path)?;
        validate_protocol_label(&origin.transformation)?;
        if let Some(digest) = &origin.generator_digest {
            let _: [u8; ADAPTER_DIGEST_BYTES] = digest
                .value
                .as_slice()
                .try_into()
                .map_err(|_| AdapterHostError::ProjectRequest)?;
        }
        if origin.generated_start_byte >= origin.generated_end_byte
            || origin.generated_end_byte > source_bytes
            || origin.origin_start_byte >= origin.origin_end_byte
            || (index != 0 && origin.generated_start_byte < previous_end)
        {
            return Err(AdapterHostError::ProjectRequest);
        }
        previous_end = origin.generated_end_byte;
    }
    Ok(())
}

fn wire_digest(value: Option<&WireContentHash>) -> Result<ContentHash, AdapterHostError> {
    let bytes: [u8; ADAPTER_DIGEST_BYTES] = value
        .ok_or(AdapterHostError::ProjectRequest)?
        .value
        .as_slice()
        .try_into()
        .map_err(|_| AdapterHostError::ProjectRequest)?;
    Ok(ContentHash::from_bytes(bytes))
}

fn fixed_wire_id<const N: usize>(value: Option<&[u8]>) -> Result<[u8; N], AdapterHostError> {
    value
        .ok_or(AdapterHostError::ProjectRequest)?
        .try_into()
        .map_err(|_| AdapterHostError::ProjectRequest)
}

fn project_adapter_binary_digest(executable: &Path) -> Result<ContentHash, AdapterHostError> {
    project_adapter_binary_digest_inner(executable, None)
}

fn project_adapter_binary_digest_with_cancellation(
    executable: &Path,
    cancellation: &Cancellation,
) -> Result<ContentHash, AdapterHostError> {
    project_adapter_binary_digest_inner(executable, Some(cancellation))
}

fn project_adapter_binary_digest_inner(
    executable: &Path,
    cancellation: Option<&Cancellation>,
) -> Result<ContentHash, AdapterHostError> {
    check_optional_cancellation(cancellation)?;
    let mut file = File::open(executable).map_err(|_| AdapterHostError::BinaryIdentity)?;
    let metadata = file
        .metadata()
        .map_err(|_| AdapterHostError::BinaryIdentity)?;
    if metadata.len() == 0 || metadata.len() > MAX_ADAPTER_EXECUTABLE_BYTES {
        return Err(AdapterHostError::BinaryIdentity);
    }
    project_adapter_binary_digest_reader(&mut file, metadata.len(), cancellation)
}

fn project_adapter_binary_digest_reader(
    reader: &mut impl Read,
    expected_bytes: u64,
    cancellation: Option<&Cancellation>,
) -> Result<ContentHash, AdapterHostError> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut observed = 0_u64;
    loop {
        check_optional_cancellation(cancellation)?;
        let read = reader
            .read(&mut buffer)
            .map_err(|_| AdapterHostError::BinaryIdentity)?;
        if read == 0 {
            break;
        }
        observed = observed
            .checked_add(u64::try_from(read).map_err(|_| AdapterHostError::BinaryIdentity)?)
            .ok_or(AdapterHostError::BinaryIdentity)?;
        if observed > MAX_ADAPTER_EXECUTABLE_BYTES {
            return Err(AdapterHostError::BinaryIdentity);
        }
        hasher.update(&buffer[..read]);
    }
    check_optional_cancellation(cancellation)?;
    if observed != expected_bytes {
        return Err(AdapterHostError::BinaryIdentity);
    }
    Ok(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}

fn check_optional_cancellation(
    cancellation: Option<&Cancellation>,
) -> Result<(), AdapterHostError> {
    cancellation.map_or(Ok(()), |cancellation| {
        cancellation.check().map_err(AdapterHostError::from)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CancellingReader {
        cancellation: Cancellation,
        emitted: bool,
    }

    impl Read for CancellingReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.emitted {
                return Ok(0);
            }
            buffer.fill(0x5a);
            self.emitted = true;
            let _ = self
                .cancellation
                .cancel(rootlight_cancel::CancellationReason::Shutdown);
            Ok(buffer.len())
        }
    }

    #[test]
    fn executable_identity_hashing_observes_cancellation_between_chunks() {
        let cancellation = Cancellation::new();
        let mut reader = CancellingReader {
            cancellation: cancellation.clone(),
            emitted: false,
        };

        let error =
            project_adapter_binary_digest_reader(&mut reader, 2 * 64 * 1024, Some(&cancellation))
                .expect_err("the second chunk observes cancellation");

        assert!(matches!(error, AdapterHostError::Cancelled(cancelled)
            if cancelled.reason() == rootlight_cancel::CancellationReason::Shutdown));
    }

    #[test]
    fn project_session_arguments_are_exact_and_bounded() {
        let arguments = [
            WALL_TIME_ARGUMENT,
            "5000",
            MEMORY_BYTES_ARGUMENT,
            "268435456",
            INPUT_BYTES_ARGUMENT,
            "1048576",
            OUTPUT_BYTES_ARGUMENT,
            "1048576",
            FILES_ARGUMENT,
            "8",
        ]
        .into_iter()
        .map(OsString::from);
        let limits = ProjectSessionLimits::parse(arguments).expect("limits are valid");
        assert_eq!(limits.wall_time_ms, 5_000);
        assert_eq!(limits.files, 8);

        let missing = [WALL_TIME_ARGUMENT, "5000"].into_iter().map(OsString::from);
        assert!(matches!(
            ProjectSessionLimits::parse(missing),
            Err(AdapterHostError::Limit)
        ));

        let noncanonical = [
            WALL_TIME_ARGUMENT,
            "05000",
            MEMORY_BYTES_ARGUMENT,
            "268435456",
            INPUT_BYTES_ARGUMENT,
            "1048576",
            OUTPUT_BYTES_ARGUMENT,
            "1048576",
            FILES_ARGUMENT,
            "8",
        ]
        .into_iter()
        .map(OsString::from);
        assert!(matches!(
            ProjectSessionLimits::parse(noncanonical),
            Err(AdapterHostError::Limit)
        ));

        let excessive = [
            WALL_TIME_ARGUMENT,
            "60001",
            MEMORY_BYTES_ARGUMENT,
            "268435456",
            INPUT_BYTES_ARGUMENT,
            "1048576",
            OUTPUT_BYTES_ARGUMENT,
            "1048576",
            FILES_ARGUMENT,
            "8",
        ]
        .into_iter()
        .map(OsString::from);
        assert!(matches!(
            ProjectSessionLimits::parse(excessive),
            Err(AdapterHostError::Limit)
        ));
    }

    #[test]
    fn protocol_paths_and_languages_reject_aliases() {
        assert!(protocol_path("src/lib.rs").is_ok());
        for path in ["../src/lib.rs", "src\\lib.rs", "/src/lib.rs", "src//lib.rs"] {
            assert!(matches!(
                protocol_path(path),
                Err(AdapterHostError::ProjectRequest)
            ));
        }
        assert_eq!(
            semantic_language("typescript").expect("language is supported"),
            SemanticProjectLanguage::TypeScript
        );
        assert!(matches!(
            semantic_language("TypeScript"),
            Err(AdapterHostError::ProjectRequest)
        ));
    }

    #[test]
    fn project_session_negotiates_every_emitted_extension() {
        let extensions = project_extensions();
        assert_eq!(
            extensions
                .iter()
                .map(|extension| extension.namespace.as_str())
                .collect::<Vec<_>>(),
            [
                FILE_IDENTITY_CLAIM_NAMESPACE,
                SYMBOL_IDENTITY_CLAIM_NAMESPACE,
                LEXICAL_EXTENSION_NAMESPACE,
            ]
        );
        assert!(extensions.iter().all(|extension| {
            extension.version == Some(ContractVersion { major: 1, minor: 0 }) && !extension.critical
        }));
    }

    #[test]
    fn project_syntax_node_limit_accounts_for_every_partition_input() {
        assert_eq!(
            project_syntax_node_limit([8_usize, 13]).expect("partition bytes are representable"),
            44
        );
        assert_eq!(
            project_syntax_node_limit([0_usize, 0]).expect("empty inputs remain representable"),
            2
        );
        assert_eq!(
            project_syntax_node_limit([MAX_SYNTAX_NODES])
                .expect("large partitions clamp to the hard ceiling"),
            MAX_SYNTAX_NODES
        );
        assert!(project_syntax_node_limit([usize::MAX, 1]).is_err());
    }

    #[test]
    fn project_execution_preserves_resource_limit_categories() {
        assert!(matches!(
            map_project_execution_error(AdapterError::RejectedRequest(
                RequestError::ProviderLimit {
                    resource: ResourceKind::ProjectSourceBytes,
                    observed: 2,
                    limit: 1,
                }
            )),
            AdapterHostError::ProjectInputLimit
        ));
        assert!(matches!(
            map_project_execution_error(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::OutputBytes,
                observed: 2,
                limit: 1,
            })),
            AdapterHostError::ProjectOutputLimit
        ));
        assert!(matches!(
            map_project_execution_error(AdapterError::InvalidReport(ReportError::ResourceLimit {
                resource: ResourceKind::ReportedMemoryBytes,
                observed: 2,
                limit: 1,
            })),
            AdapterHostError::ProjectMemoryLimit
        ));
        assert!(matches!(
            map_project_execution_error(AdapterError::Sink(SinkError::AllocationFailed)),
            AdapterHostError::ProjectMemoryLimit
        ));
    }
}
