//! Fail-closed admission and hostile-output validation for adapter processes.
//!
//! No deep adapter is spawned until every required platform control is backed
//! by auditable enforcement. External declarative results still cross the same
//! bounded, generation-bound normalized-IR validation boundary.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId, content_hash};
use rootlight_ir::{
    ExtensionCriticality, ExtensionSupport, IrDocument, IrDocumentDecodeError,
    IrDocumentValidationError, IrLimits, NormalizedIrDocument, canonicalize_ir_document,
    decode_ir_document,
};
use rootlight_protocol::{
    adapter_contract::{AdapterContractError, NegotiatedSession, decode_adapter_frame},
    generated::{
        adapter::v1::{AnalysisRequest, AnalysisResult, adapter_frame},
        common::v1::{ContractVersion, ExtensionDescriptor},
    },
};

const NORMALIZED_IR_CAPABILITY: &str = "normalized_ir";

/// Controls required before Rootlight may activate an untrusted deep adapter.
pub const REQUIRED_SANDBOX_CONTROLS: [SandboxControl; 9] = [
    SandboxControl::FilesystemView,
    SandboxControl::TemporaryDirectory,
    SandboxControl::NetworkEgress,
    SandboxControl::ProcessCreation,
    SandboxControl::Memory,
    SandboxControl::Cpu,
    SandboxControl::Handles,
    SandboxControl::DynamicLibrarySearch,
    SandboxControl::DescendantCleanup,
];

/// A security property that must be enforced outside the daemon process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum SandboxControl {
    /// The process can observe only approved immutable inputs.
    FilesystemView,
    /// Temporary output is private, bounded, and independently removable.
    TemporaryDirectory,
    /// DNS and outbound network access are denied.
    NetworkEgress,
    /// Child-process creation is denied or owned by the host.
    ProcessCreation,
    /// A hard memory ceiling is enforced by the operating system.
    Memory,
    /// CPU consumption is bounded independently of cooperative deadlines.
    Cpu,
    /// File descriptors or handles are allow-listed and bounded.
    Handles,
    /// Dynamic-library lookup cannot reach ambient attacker-controlled paths.
    DynamicLibrarySearch,
    /// Every descendant is terminated when the host scope ends.
    DescendantCleanup,
}

/// Required platform family reported by the isolation probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostPlatform {
    /// Linux sandbox backend.
    Linux,
    /// macOS sandbox backend.
    MacOs,
    /// Windows sandbox backend.
    Windows,
    /// A platform without a mandatory Rootlight isolation profile.
    Unsupported,
}

/// Observed enforcement state for one required control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlEvidence {
    enforced: bool,
    reason_code: &'static str,
}

impl ControlEvidence {
    /// Returns whether the platform, rather than the adapter, enforces the control.
    #[must_use]
    pub const fn is_enforced(self) -> bool {
        self.enforced
    }

    /// Returns a stable source-free explanation code.
    #[must_use]
    pub const fn reason_code(self) -> &'static str {
        self.reason_code
    }

    const fn unavailable(reason_code: &'static str) -> Self {
        Self {
            enforced: false,
            reason_code,
        }
    }

    #[cfg(test)]
    const fn enforced() -> Self {
        Self {
            enforced: true,
            reason_code: "enforced_by_test_backend",
        }
    }
}

/// Immutable capability report produced before any adapter process is started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationReport {
    platform: HostPlatform,
    controls: BTreeMap<SandboxControl, ControlEvidence>,
}

impl IsolationReport {
    /// Probes the current platform conservatively.
    ///
    /// The initial backend intentionally reports every native enforcement
    /// control unavailable. This makes the documented structural-tier fallback
    /// active until audited Linux, macOS, and Windows backends land.
    #[must_use]
    pub fn current() -> Self {
        let (platform, reason) = current_platform_and_reason();
        Self {
            platform,
            controls: REQUIRED_SANDBOX_CONTROLS
                .into_iter()
                .map(|control| (control, ControlEvidence::unavailable(reason)))
                .collect(),
        }
    }

    /// Returns the detected platform family.
    #[must_use]
    pub const fn platform(&self) -> HostPlatform {
        self.platform
    }

    /// Returns evidence for a required control.
    #[must_use]
    pub fn control(&self, control: SandboxControl) -> Option<ControlEvidence> {
        self.controls.get(&control).copied()
    }

    /// Returns whether every required control is independently enforced.
    #[must_use]
    pub fn permits_deep_adapter(&self) -> bool {
        REQUIRED_SANDBOX_CONTROLS.iter().all(|control| {
            self.controls
                .get(control)
                .is_some_and(|evidence| evidence.is_enforced())
        })
    }

    #[cfg(test)]
    fn fully_enforced(platform: HostPlatform) -> Self {
        Self {
            platform,
            controls: REQUIRED_SANDBOX_CONTROLS
                .into_iter()
                .map(|control| (control, ControlEvidence::enforced()))
                .collect(),
        }
    }
}

/// Fail-closed tier decision made before process creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdapterActivation {
    /// All mandatory controls are enforced; a backend may create the process.
    IsolatedDeep,
    /// Rootlight must retain the in-process structural parser tier.
    StructuralFallback,
}

/// Evaluates the mandatory isolation controls without consulting adapter input.
#[must_use]
pub fn evaluate_adapter_activation(report: &IsolationReport) -> AdapterActivation {
    if report.permits_deep_adapter() {
        AdapterActivation::IsolatedDeep
    } else {
        AdapterActivation::StructuralFallback
    }
}

/// Generation-bound request identity retained across an untrusted process call.
#[derive(Debug, PartialEq, Eq)]
pub struct PendingAnalysis {
    request_id: [u8; 16],
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    source_digest: ContentHash,
    source_bytes: u64,
    language: String,
    build_context: ContentHash,
}

impl PendingAnalysis {
    /// Returns the immutable repository identity.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the immutable generation identity.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the immutable file identity.
    #[must_use]
    pub const fn file(&self) -> FileId {
        self.file
    }
}

/// Validates an outbound request and binds the response to its immutable input.
///
/// # Errors
///
/// Returns [`AdapterHostError`] for cancellation, malformed protocol fields,
/// an incorrect source digest, or an invalid stable identifier.
pub fn prepare_analysis(
    session: &NegotiatedSession,
    request: &AnalysisRequest,
    cancellation: &Cancellation,
) -> Result<PendingAnalysis, AdapterHostError> {
    cancellation.check()?;
    if !session.has_capability(NORMALIZED_IR_CAPABILITY) {
        return Err(AdapterHostError::CapabilityMismatch);
    }
    session.validate_analysis_request(request)?;
    let expected_source_digest =
        request
            .source_digest
            .as_ref()
            .ok_or(AdapterHostError::Protocol(
                AdapterContractError::MissingField,
            ))?;
    let source_digest = content_hash(&request.source);
    if expected_source_digest.value.as_slice() != source_digest.as_bytes() {
        return Err(AdapterHostError::DigestMismatch);
    }
    cancellation.check()?;
    let source_bytes = u64::try_from(request.source.len()).map_err(|_| AdapterHostError::Limit)?;
    let build_context = ContentHash::from_bytes(fixed_id(
        &request
            .build_context
            .as_ref()
            .ok_or(AdapterHostError::Protocol(
                AdapterContractError::MissingField,
            ))?
            .value,
    )?);
    Ok(PendingAnalysis {
        request_id: fixed_id(&request.request_id)?,
        repository: RepositoryId::from_bytes(fixed_id(
            &request
                .repository
                .as_ref()
                .ok_or(AdapterHostError::Protocol(
                    AdapterContractError::MissingField,
                ))?
                .value,
        )?),
        generation: GenerationId::from_bytes(fixed_id(
            &request
                .generation
                .as_ref()
                .ok_or(AdapterHostError::Protocol(
                    AdapterContractError::MissingField,
                ))?
                .value,
        )?),
        file: FileId::from_bytes(fixed_id(
            &request
                .file
                .as_ref()
                .ok_or(AdapterHostError::Protocol(
                    AdapterContractError::MissingField,
                ))?
                .value,
        )?),
        source_digest,
        source_bytes,
        language: request.language.clone(),
        build_context,
    })
}

/// Decodes, correlates, hashes, and canonicalizes one hostile adapter result.
///
/// This boundary is also used for explicitly supplied external semantic
/// context. Passing it does not imply that a platform sandbox was enforced.
///
/// # Errors
///
/// Returns [`AdapterHostError`] for cancellation, malformed or unexpected
/// frames, correlation or digest mismatch, bounded IR decode failure, invalid
/// normalized facts, or repository/generation substitution.
pub fn validate_analysis_result(
    session: &NegotiatedSession,
    pending: PendingAnalysis,
    encoded_frame: &[u8],
    supported_extensions: &ExtensionSupport,
    cancellation: &Cancellation,
) -> Result<NormalizedIrDocument, AdapterHostError> {
    cancellation.check()?;
    let frame = decode_adapter_frame(encoded_frame)?;
    let result = match frame.message {
        Some(adapter_frame::Message::AnalysisResult(result)) => result,
        _ => return Err(AdapterHostError::UnexpectedFrame),
    };
    validate_result_correlation(session, &pending, &result)?;
    cancellation.check()?;

    let output_digest = result
        .output_digest
        .as_ref()
        .ok_or(AdapterHostError::Protocol(
            AdapterContractError::MissingField,
        ))?;
    if output_digest.value.as_slice() != content_hash(&result.normalized_ir).as_bytes() {
        return Err(AdapterHostError::DigestMismatch);
    }
    cancellation.check()?;
    let mut limits = IrLimits::default();
    limits.max_document_bytes = usize::try_from(session.limits().output_bytes)
        .map_err(|_| AdapterHostError::Limit)?
        .min(limits.max_document_bytes);
    let decoded = decode_ir_document(&result.normalized_ir, &limits, supported_extensions)?;
    cancellation.check()?;
    let IrDocument::NormalizedV1_1(document) = decoded else {
        return Err(AdapterHostError::UnsupportedIr);
    };
    let document = canonicalize_ir_document(document, &limits, supported_extensions)?;
    cancellation.check()?;
    validate_document_binding(session, &pending, &document)?;
    Ok(document)
}

fn validate_result_correlation(
    session: &NegotiatedSession,
    pending: &PendingAnalysis,
    result: &AnalysisResult,
) -> Result<(), AdapterHostError> {
    session.validate_analysis_result(result)?;
    if result.request_id.as_slice() != pending.request_id {
        return Err(AdapterHostError::RequestMismatch);
    }
    Ok(())
}

fn validate_document_binding(
    session: &NegotiatedSession,
    pending: &PendingAnalysis,
    document: &NormalizedIrDocument,
) -> Result<(), AdapterHostError> {
    if document.repository != pending.repository || document.generation != pending.generation {
        return Err(AdapterHostError::ContextMismatch);
    }
    let [file] = document.files.as_slice() else {
        return Err(AdapterHostError::SourceContextMismatch);
    };
    if file.id != pending.file
        || file.content_hash != pending.source_digest
        || file.byte_length != pending.source_bytes
        || file.language != pending.language
    {
        return Err(AdapterHostError::SourceContextMismatch);
    }

    let adapter = session.adapter();
    let adapter_digest = ContentHash::from_bytes(fixed_id(&adapter.source_digest)?);
    for provenance in &document.provenance {
        if provenance.producer.name() != adapter.name
            || provenance.producer.version() != adapter.version
            || provenance.binary_digest != adapter_digest
        {
            return Err(AdapterHostError::ProvenanceMismatch);
        }
        if provenance.language != pending.language
            || provenance.build_context.digest() != pending.build_context
        {
            return Err(AdapterHostError::ContextMismatch);
        }
    }
    for extension in &document.extensions {
        let version = parse_extension_version(&extension.version)
            .ok_or(AdapterHostError::ExtensionMismatch)?;
        let descriptor = ExtensionDescriptor {
            namespace: extension.namespace.clone(),
            version: Some(version),
            critical: extension.criticality == ExtensionCriticality::Critical,
        };
        if !session.has_extension(&descriptor) {
            return Err(AdapterHostError::ExtensionMismatch);
        }
    }
    Ok(())
}

fn parse_extension_version(version: &str) -> Option<ContractVersion> {
    let mut components = version.split('.');
    let major = components.next()?.parse().ok()?;
    let minor = components.next()?.parse().ok()?;
    if major == 0 || components.next().is_some() {
        return None;
    }
    Some(ContractVersion { major, minor })
}

fn fixed_id<const N: usize>(bytes: &[u8]) -> Result<[u8; N], AdapterHostError> {
    bytes.try_into().map_err(|_| AdapterHostError::Identifier)
}

const fn current_platform_and_reason() -> (HostPlatform, &'static str) {
    if cfg!(target_os = "linux") {
        (
            HostPlatform::Linux,
            "linux_native_enforcement_not_implemented",
        )
    } else if cfg!(target_os = "macos") {
        (
            HostPlatform::MacOs,
            "macos_native_enforcement_not_implemented",
        )
    } else if cfg!(target_os = "windows") {
        (
            HostPlatform::Windows,
            "windows_native_enforcement_not_implemented",
        )
    } else {
        (HostPlatform::Unsupported, "unsupported_platform")
    }
}

/// Source-free failure at the adapter trust boundary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AdapterHostError {
    /// Cooperative cancellation or a monotonic deadline won.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    /// The bounded adapter protocol was malformed or incompatible.
    #[error(transparent)]
    Protocol(#[from] AdapterContractError),
    /// The frame was valid but not an analysis result.
    #[error("adapter returned an unexpected frame")]
    UnexpectedFrame,
    /// The session did not negotiate normalized IR output.
    #[error("adapter normalized IR capability was not negotiated")]
    CapabilityMismatch,
    /// The result belonged to another request.
    #[error("adapter result request identity did not match")]
    RequestMismatch,
    /// A declared content digest did not match the bytes.
    #[error("adapter content digest did not match")]
    DigestMismatch,
    /// A stable identifier had an invalid byte width.
    #[error("adapter stable identifier was malformed")]
    Identifier,
    /// A negotiated numeric limit cannot be represented safely.
    #[error("adapter resource limit was invalid")]
    Limit,
    /// The normalized IR envelope could not be decoded within its limits.
    #[error("adapter normalized IR could not be decoded")]
    Decode(#[from] IrDocumentDecodeError),
    /// The normalized IR facts violated the canonical contract.
    #[error("adapter normalized IR was invalid")]
    Validation(#[from] IrDocumentValidationError),
    /// Legacy IR is never accepted from an isolated adapter.
    #[error("adapter returned an unsupported IR contract")]
    UnsupportedIr,
    /// The result attempted to substitute another repository or generation.
    #[error("adapter result context did not match")]
    ContextMismatch,
    /// The result did not describe exactly the requested immutable file.
    #[error("adapter result source context did not match")]
    SourceContextMismatch,
    /// Result provenance did not identify the negotiated adapter binary.
    #[error("adapter result provenance did not match")]
    ProvenanceMismatch,
    /// A returned extension was not negotiated for the session.
    #[error("adapter result extension was not negotiated")]
    ExtensionMismatch,
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{FactId, content_hash};
    use rootlight_ir::{
        AnalysisTier, BuildContextIdentity, ExtensionEnvelope, FactEvidence, FileRecord,
        ProducerIdentity, ProducerKind, ProvenanceRecord, SourceRef, SourceSpan,
    };
    use rootlight_protocol::{
        adapter_contract::{
            ADAPTER_PROTOCOL_MAJOR, CURRENT_ADAPTER_PROTOCOL_MINOR, ValidatedAdvertisement,
            encode_adapter_frame,
        },
        generated::{
            adapter::v1::{
                AdapterFrame, AdapterIdentity, AdapterTrustLevel, AnalysisResult,
                CapabilityAdvertisement, ResourceLimits, SessionRequirements,
            },
            common::v1::{
                ContentHash as WireContentHash, ContractVersion, FileId as WireFileId,
                GenerationId as WireGenerationId, RepositoryId as WireRepositoryId, VersionRange,
            },
        },
    };

    use super::*;

    #[test]
    fn current_platform_fails_closed_before_process_creation() {
        let report = IsolationReport::current();
        assert_eq!(
            evaluate_adapter_activation(&report),
            AdapterActivation::StructuralFallback
        );
        for control in REQUIRED_SANDBOX_CONTROLS {
            let evidence = report.control(control).expect("control is reported");
            assert!(!evidence.is_enforced());
            assert!(!evidence.reason_code().is_empty());
        }
        let complete = IsolationReport::fully_enforced(report.platform());
        assert_eq!(
            evaluate_adapter_activation(&complete),
            AdapterActivation::IsolatedDeep
        );
    }

    #[test]
    fn external_result_is_digest_checked_and_generation_bound() {
        let (session, request) = session_and_request();
        let cancellation = Cancellation::new();
        let pending =
            prepare_analysis(&session, &request, &cancellation).expect("request is accepted");
        let document = normalized_document(&session, &pending);
        let payload = serde_json::to_vec(&document).expect("document encodes");
        let result = result_frame(&session, &pending, payload);
        let expected_repository = pending.repository();
        let expected_generation = pending.generation();

        let validated = validate_analysis_result(
            &session,
            pending,
            &result,
            &ExtensionSupport::default(),
            &cancellation,
        )
        .expect("external result passes the hostile boundary");
        assert_eq!(validated.repository, expected_repository);
        assert_eq!(validated.generation, expected_generation);
    }

    #[test]
    fn hostile_digest_context_and_cancellation_fail_without_source_leakage() {
        let (session, request) = session_and_request();
        let cancellation = Cancellation::new();
        let pending =
            prepare_analysis(&session, &request, &cancellation).expect("request is accepted");
        let document = normalized_document(&session, &pending);
        let payload = serde_json::to_vec(&document).expect("document encodes");
        let encoded = result_frame(&session, &pending, payload);
        let mut frame = decode_adapter_frame(&encoded).expect("fixture frame decodes");
        let Some(adapter_frame::Message::AnalysisResult(result)) = &mut frame.message else {
            panic!("fixture contains an analysis result");
        };
        result
            .output_digest
            .as_mut()
            .expect("fixture digest exists")
            .value[0] ^= 1;
        let encoded = encode_adapter_frame(&frame).expect("mutated frame encodes");
        let error = validate_analysis_result(
            &session,
            pending,
            &encoded,
            &ExtensionSupport::default(),
            &cancellation,
        )
        .expect_err("wrong digest is rejected");
        assert!(matches!(error, AdapterHostError::DigestMismatch));
        assert!(!error.to_string().contains("source"));

        let pending =
            prepare_analysis(&session, &request, &cancellation).expect("request is accepted");
        let substituted = normalized_document_with_context(
            &session,
            &pending,
            GenerationId::from_bytes([8; 20]),
            pending.file(),
            pending.source_digest,
        );
        let encoded = result_frame(
            &session,
            &pending,
            serde_json::to_vec(&substituted).expect("substituted document encodes"),
        );
        assert!(matches!(
            validate_analysis_result(
                &session,
                pending,
                &encoded,
                &ExtensionSupport::default(),
                &cancellation
            ),
            Err(AdapterHostError::ContextMismatch)
        ));

        let pending =
            prepare_analysis(&session, &request, &cancellation).expect("request is accepted");
        let encoded = result_frame(
            &session,
            &pending,
            serde_json::to_vec(&normalized_document(&session, &pending)).expect("document encodes"),
        );
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            validate_analysis_result(
                &session,
                pending,
                &encoded,
                &ExtensionSupport::default(),
                &cancellation
            ),
            Err(AdapterHostError::Cancelled(_))
        ));
    }

    #[test]
    fn result_rejects_source_provenance_and_extension_substitution() {
        let (session, request) = session_and_request();
        let cancellation = Cancellation::new();

        let pending =
            prepare_analysis(&session, &request, &cancellation).expect("request is accepted");
        let substituted = normalized_document_with_context(
            &session,
            &pending,
            pending.generation(),
            pending.file(),
            content_hash(b"different immutable source"),
        );
        let encoded = result_frame(
            &session,
            &pending,
            serde_json::to_vec(&substituted).expect("substituted document encodes"),
        );
        assert!(matches!(
            validate_analysis_result(
                &session,
                pending,
                &encoded,
                &ExtensionSupport::default(),
                &cancellation
            ),
            Err(AdapterHostError::SourceContextMismatch)
        ));

        let pending =
            prepare_analysis(&session, &request, &cancellation).expect("request is accepted");
        let mut substituted = normalized_document(&session, &pending);
        substituted.provenance[0].producer =
            ProducerIdentity::new("impostor", "1.0.0", pending.build_context)
                .expect("fixture producer is valid");
        let encoded = result_frame(
            &session,
            &pending,
            serde_json::to_vec(&substituted).expect("substituted document encodes"),
        );
        assert!(matches!(
            validate_analysis_result(
                &session,
                pending,
                &encoded,
                &ExtensionSupport::default(),
                &cancellation
            ),
            Err(AdapterHostError::ProvenanceMismatch)
        ));

        let pending =
            prepare_analysis(&session, &request, &cancellation).expect("request is accepted");
        let mut substituted = normalized_document(&session, &pending);
        let evidence = substituted.files[0].evidence.clone();
        substituted.extensions.push(ExtensionEnvelope {
            id: FactId::from_bytes([8; 20]),
            repository: pending.repository(),
            generation: pending.generation(),
            namespace: "dev.rootlight.unnegotiated".to_owned(),
            version: "1.0".to_owned(),
            criticality: ExtensionCriticality::Noncritical,
            payload: "{}".to_owned(),
            provenance: FactId::from_bytes([7; 20]),
            evidence,
        });
        let encoded = result_frame(
            &session,
            &pending,
            serde_json::to_vec(&substituted).expect("substituted document encodes"),
        );
        assert!(matches!(
            validate_analysis_result(
                &session,
                pending,
                &encoded,
                &ExtensionSupport::default(),
                &cancellation
            ),
            Err(AdapterHostError::ExtensionMismatch)
        ));
    }

    fn session_and_request() -> (NegotiatedSession, AnalysisRequest) {
        let identity = AdapterIdentity {
            name: "fixture-adapter".to_owned(),
            version: "1.0.0".to_owned(),
            source_digest: vec![9; 32],
        };
        let limits = ResourceLimits {
            wall_time_ms: 10_000,
            cpu_time_ms: 5_000,
            memory_bytes: 64 * 1024 * 1024,
            input_bytes: 1024 * 1024,
            output_bytes: 1024 * 1024,
            files: 1,
            processes: 1,
            handles: 16,
            retries: 0,
        };
        let advertisement = ValidatedAdvertisement::validate(CapabilityAdvertisement {
            adapter: Some(identity.clone()),
            supported_protocols: Some(VersionRange {
                minimum: Some(ContractVersion {
                    major: ADAPTER_PROTOCOL_MAJOR,
                    minor: CURRENT_ADAPTER_PROTOCOL_MINOR,
                }),
                maximum: Some(ContractVersion {
                    major: ADAPTER_PROTOCOL_MAJOR,
                    minor: CURRENT_ADAPTER_PROTOCOL_MINOR,
                }),
            }),
            capabilities: vec!["normalized_ir".to_owned()],
            extensions: Vec::new(),
            trust_level: AdapterTrustLevel::FirstParty as i32,
            hard_limits: Some(limits),
            supports_cancellation: true,
        })
        .expect("advertisement is valid");
        let session = advertisement
            .negotiate(SessionRequirements {
                session_id: vec![1; 16],
                selected_protocol: Some(advertisement.selected_protocol()),
                expected_adapter: Some(identity),
                required_capabilities: vec!["normalized_ir".to_owned()],
                required_extensions: Vec::new(),
                granted_limits: Some(limits),
                maximum_trust: AdapterTrustLevel::FirstParty as i32,
                require_cancellation: true,
            })
            .expect("session negotiates");
        let source = b"fn main() {}".to_vec();
        (
            session,
            AnalysisRequest {
                session_id: vec![1; 16],
                request_id: vec![2; 16],
                repository: Some(WireRepositoryId { value: vec![3; 16] }),
                generation: Some(WireGenerationId { value: vec![4; 20] }),
                file: Some(WireFileId { value: vec![5; 20] }),
                language: "rust".to_owned(),
                build_context: Some(WireContentHash { value: vec![6; 32] }),
                source_digest: Some(WireContentHash {
                    value: content_hash(&source).as_bytes().to_vec(),
                }),
                source,
            },
        )
    }

    fn result_frame(
        session: &NegotiatedSession,
        pending: &PendingAnalysis,
        normalized_ir: Vec<u8>,
    ) -> Vec<u8> {
        let output_digest = content_hash(&normalized_ir);
        let frame = AdapterFrame {
            message: Some(adapter_frame::Message::AnalysisResult(AnalysisResult {
                session_id: session.session_id().to_vec(),
                request_id: pending.request_id.to_vec(),
                normalized_ir,
                output_digest: Some(WireContentHash {
                    value: output_digest.as_bytes().to_vec(),
                }),
            })),
        };
        encode_adapter_frame(&frame).expect("result frame encodes")
    }

    fn normalized_document(
        session: &NegotiatedSession,
        pending: &PendingAnalysis,
    ) -> NormalizedIrDocument {
        normalized_document_with_context(
            session,
            pending,
            pending.generation(),
            pending.file(),
            pending.source_digest,
        )
    }

    fn normalized_document_with_context(
        session: &NegotiatedSession,
        pending: &PendingAnalysis,
        generation: GenerationId,
        file: FileId,
        source_digest: ContentHash,
    ) -> NormalizedIrDocument {
        let source = SourceRef::new(
            pending.repository(),
            generation,
            SourceSpan::new(file, 0, pending.source_bytes).expect("fixture span is ordered"),
            source_digest,
            None,
        );
        let provenance = FactId::from_bytes([7; 20]);
        let adapter_digest = ContentHash::from_bytes(
            session
                .adapter()
                .source_digest
                .as_slice()
                .try_into()
                .expect("fixture adapter digest has canonical width"),
        );
        let mut document = NormalizedIrDocument::empty(pending.repository(), generation);
        document.files.push(FileRecord {
            id: file,
            repository: pending.repository(),
            generation,
            path: "src/lib.rs".to_owned(),
            path_locator: None,
            content_hash: source_digest,
            byte_length: pending.source_bytes,
            language: pending.language.clone(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance,
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation: Vec::new(),
            },
        });
        document.provenance.push(ProvenanceRecord {
            id: provenance,
            repository: pending.repository(),
            generation,
            producer_kind: ProducerKind::Compiler,
            producer: ProducerIdentity::new(
                &session.adapter().name,
                &session.adapter().version,
                pending.build_context,
            )
            .expect("fixture producer is valid"),
            binary_digest: adapter_digest,
            frontend_version: Some("fixture-frontend-1".to_owned()),
            language: pending.language.clone(),
            tier: AnalysisTier::TierA,
            build_context: BuildContextIdentity::new(pending.build_context),
            input_sources: vec![source.clone()],
            evidence_sources: vec![source],
            derivation_parents: Vec::new(),
            rule: None,
        });
        document
    }
}
