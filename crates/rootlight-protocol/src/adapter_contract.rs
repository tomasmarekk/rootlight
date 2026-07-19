//! Bounded negotiation and frame validation for isolated adapter protocol v1.
//!
//! Raw protobuf messages cross a hostile process boundary only after these
//! source-redacted structural, compatibility, identity, and quota checks.

use std::collections::{BTreeMap, BTreeSet};

use prost::Message;

use crate::generated::{
    adapter::v1::{
        AdapterFrame, AdapterIdentity, AdapterTrustLevel, AnalysisRequest, AnalysisResult,
        CancelRequest, CapabilityAdvertisement, ResourceLimits, SessionRequirements, adapter_frame,
    },
    common::v1::{ContractVersion, ExtensionDescriptor, VersionRange},
};

/// Adapter protocol major supported by this host.
pub const ADAPTER_PROTOCOL_MAJOR: u32 = 1;
/// Earliest adapter protocol minor supported by this host.
pub const MINIMUM_ADAPTER_PROTOCOL_MINOR: u32 = 1;
/// Latest adapter protocol minor supported by this host.
pub const CURRENT_ADAPTER_PROTOCOL_MINOR: u32 = 1;
/// Maximum encoded adapter frame accepted from a child process.
pub const MAX_ADAPTER_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// Maximum advertised capabilities or extensions.
pub const MAX_ADAPTER_DECLARATIONS: usize = 64;
/// Canonical session and request identifier length.
pub const ADAPTER_NONCE_BYTES: usize = 16;
/// Canonical content digest length.
pub const ADAPTER_DIGEST_BYTES: usize = 32;

const MAX_LABEL_BYTES: usize = 128;
const MAX_WALL_TIME_MS: u64 = 24 * 60 * 60 * 1_000;
const MAX_CPU_TIME_MS: u64 = 24 * 60 * 60 * 1_000;
const MAX_MEMORY_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FILES: u32 = 65_536;
const MAX_PROCESSES: u32 = 256;
const MAX_HANDLES: u32 = 65_536;
const MAX_RETRIES: u32 = 8;

/// Validated child advertisement with a host-selected protocol version.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedAdvertisement {
    advertisement: CapabilityAdvertisement,
    identity: AdapterIdentity,
    selected_protocol: ContractVersion,
    capabilities: BTreeSet<String>,
    extensions: BTreeMap<String, ExtensionDescriptor>,
    trust_level: AdapterTrustLevel,
    hard_limits: ResourceLimits,
}

impl ValidatedAdvertisement {
    /// Validates a hostile capability advertisement and selects the newest
    /// mutually supported protocol minor.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterContractError`] when identity, version range,
    /// declarations, trust, cancellation, or resource ceilings are invalid.
    pub fn validate(advertisement: CapabilityAdvertisement) -> Result<Self, AdapterContractError> {
        let identity = advertisement
            .adapter
            .as_ref()
            .ok_or(AdapterContractError::MissingField)?;
        validate_identity(identity)?;
        let selected_protocol = select_protocol(
            advertisement
                .supported_protocols
                .as_ref()
                .ok_or(AdapterContractError::MissingField)?,
        )?;
        let capabilities = validate_capabilities(&advertisement.capabilities)?;
        let extensions = validate_extensions(&advertisement.extensions)?;
        let trust_level = AdapterTrustLevel::try_from(advertisement.trust_level)
            .map_err(|_| AdapterContractError::InvalidTrust)?;
        if trust_level == AdapterTrustLevel::Unspecified {
            return Err(AdapterContractError::InvalidTrust);
        }
        let hard_limits = advertisement
            .hard_limits
            .ok_or(AdapterContractError::MissingField)?;
        validate_limits(&hard_limits)?;
        if !advertisement.supports_cancellation {
            return Err(AdapterContractError::CancellationRequired);
        }
        let identity = identity.clone();
        Ok(Self {
            advertisement,
            identity,
            selected_protocol,
            capabilities,
            extensions,
            trust_level,
            hard_limits,
        })
    }

    /// Returns the validated adapter binary identity.
    #[must_use]
    pub const fn identity(&self) -> &AdapterIdentity {
        &self.identity
    }

    /// Returns the selected protocol contract.
    #[must_use]
    pub const fn selected_protocol(&self) -> ContractVersion {
        self.selected_protocol
    }

    /// Returns the validated adapter trust origin.
    #[must_use]
    pub const fn trust_level(&self) -> AdapterTrustLevel {
        self.trust_level
    }

    /// Returns the child-advertised hard resource ceiling.
    #[must_use]
    pub const fn hard_limits(&self) -> &ResourceLimits {
        &self.hard_limits
    }

    /// Negotiates exact host requirements against this advertisement.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterContractError`] for session, identity, protocol,
    /// capability, extension, trust, quota, or cancellation mismatch.
    pub fn negotiate(
        &self,
        requirements: SessionRequirements,
    ) -> Result<NegotiatedSession, AdapterContractError> {
        validate_nonce(&requirements.session_id)?;
        if requirements.selected_protocol.as_ref() != Some(&self.selected_protocol) {
            return Err(AdapterContractError::ProtocolMismatch);
        }
        let expected = requirements
            .expected_adapter
            .as_ref()
            .ok_or(AdapterContractError::MissingField)?;
        validate_identity(expected)?;
        if expected != self.identity() {
            return Err(AdapterContractError::IdentityMismatch);
        }
        let required_capabilities = validate_capabilities(&requirements.required_capabilities)?;
        if !required_capabilities.is_subset(&self.capabilities) {
            return Err(AdapterContractError::CapabilityMismatch);
        }
        let required_extensions = validate_extensions(&requirements.required_extensions)?;
        for (namespace, required) in &required_extensions {
            if self.extensions.get(namespace) != Some(required) {
                return Err(AdapterContractError::ExtensionMismatch);
            }
        }
        let maximum_trust = AdapterTrustLevel::try_from(requirements.maximum_trust)
            .map_err(|_| AdapterContractError::InvalidTrust)?;
        if maximum_trust == AdapterTrustLevel::Unspecified
            || trust_rank(self.trust_level) > trust_rank(maximum_trust)
        {
            return Err(AdapterContractError::TrustMismatch);
        }
        let granted_limits = requirements
            .granted_limits
            .ok_or(AdapterContractError::MissingField)?;
        validate_limits(&granted_limits)?;
        if !limits_fit(&granted_limits, &self.hard_limits) {
            return Err(AdapterContractError::QuotaMismatch);
        }
        if requirements.require_cancellation && !self.advertisement.supports_cancellation {
            return Err(AdapterContractError::CancellationRequired);
        }
        Ok(NegotiatedSession {
            session_id: requirements.session_id,
            adapter: expected.clone(),
            protocol: self.selected_protocol,
            capabilities: required_capabilities,
            extensions: required_extensions,
            limits: granted_limits,
            trust_level: self.trust_level,
        })
    }
}

/// Immutable negotiated contract for one isolated adapter process.
#[derive(Debug, Clone, PartialEq)]
pub struct NegotiatedSession {
    session_id: Vec<u8>,
    adapter: AdapterIdentity,
    protocol: ContractVersion,
    capabilities: BTreeSet<String>,
    extensions: BTreeMap<String, ExtensionDescriptor>,
    limits: ResourceLimits,
    trust_level: AdapterTrustLevel,
}

impl NegotiatedSession {
    /// Returns the opaque process-session identity.
    #[must_use]
    pub fn session_id(&self) -> &[u8] {
        &self.session_id
    }

    /// Returns the negotiated adapter binary identity.
    #[must_use]
    pub const fn adapter(&self) -> &AdapterIdentity {
        &self.adapter
    }

    /// Returns the selected protocol contract.
    #[must_use]
    pub const fn protocol(&self) -> ContractVersion {
        self.protocol
    }

    /// Returns the granted resource ceilings.
    #[must_use]
    pub const fn limits(&self) -> &ResourceLimits {
        &self.limits
    }

    /// Returns the negotiated trust origin.
    #[must_use]
    pub const fn trust_level(&self) -> AdapterTrustLevel {
        self.trust_level
    }

    /// Returns whether a capability was explicitly negotiated.
    #[must_use]
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }

    /// Returns whether an exact extension namespace and version were negotiated.
    #[must_use]
    pub fn has_extension(&self, extension: &ExtensionDescriptor) -> bool {
        self.extensions.get(&extension.namespace) == Some(extension)
    }

    /// Validates one analysis request against the negotiated immutable session.
    ///
    /// Content digest equality and normalized-IR semantics are deliberately
    /// revalidated by the host crate, which owns hashing and IR dependencies.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterContractError`] when identity or input bounds differ
    /// from the negotiated contract.
    pub fn validate_analysis_request(
        &self,
        request: &AnalysisRequest,
    ) -> Result<(), AdapterContractError> {
        if request.encoded_len() > MAX_ADAPTER_FRAME_BYTES {
            return Err(AdapterContractError::FrameSize);
        }
        self.validate_message_identity(&request.session_id, &request.request_id)?;
        validate_fixed_bytes(
            request
                .repository
                .as_ref()
                .ok_or(AdapterContractError::MissingField)?
                .value
                .as_slice(),
            16,
        )?;
        validate_fixed_bytes(
            request
                .generation
                .as_ref()
                .ok_or(AdapterContractError::MissingField)?
                .value
                .as_slice(),
            20,
        )?;
        validate_fixed_bytes(
            request
                .file
                .as_ref()
                .ok_or(AdapterContractError::MissingField)?
                .value
                .as_slice(),
            20,
        )?;
        validate_protocol_label(&request.language)?;
        validate_digest(
            request
                .build_context
                .as_ref()
                .ok_or(AdapterContractError::MissingField)?
                .value
                .as_slice(),
        )?;
        validate_digest(
            request
                .source_digest
                .as_ref()
                .ok_or(AdapterContractError::MissingField)?
                .value
                .as_slice(),
        )?;
        if u64::try_from(request.source.len()).map_err(|_| AdapterContractError::QuotaMismatch)?
            > self.limits.input_bytes
        {
            return Err(AdapterContractError::QuotaMismatch);
        }
        Ok(())
    }

    /// Validates one adapter result before semantic IR decoding.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterContractError`] when request correlation, output
    /// ceiling, or declared digest shape is invalid.
    pub fn validate_analysis_result(
        &self,
        result: &AnalysisResult,
    ) -> Result<(), AdapterContractError> {
        if result.encoded_len() > MAX_ADAPTER_FRAME_BYTES {
            return Err(AdapterContractError::FrameSize);
        }
        self.validate_message_identity(&result.session_id, &result.request_id)?;
        validate_digest(
            result
                .output_digest
                .as_ref()
                .ok_or(AdapterContractError::MissingField)?
                .value
                .as_slice(),
        )?;
        if u64::try_from(result.normalized_ir.len())
            .map_err(|_| AdapterContractError::QuotaMismatch)?
            > self.limits.output_bytes
        {
            return Err(AdapterContractError::QuotaMismatch);
        }
        Ok(())
    }

    /// Validates one cooperative cancellation correlation.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterContractError`] when the session or request identity
    /// is malformed or belongs to another session.
    pub fn validate_cancel(&self, cancel: &CancelRequest) -> Result<(), AdapterContractError> {
        self.validate_message_identity(&cancel.session_id, &cancel.request_id)
    }

    fn validate_message_identity(
        &self,
        session_id: &[u8],
        request_id: &[u8],
    ) -> Result<(), AdapterContractError> {
        validate_nonce(session_id)?;
        validate_nonce(request_id)?;
        if session_id != self.session_id {
            return Err(AdapterContractError::SessionMismatch);
        }
        Ok(())
    }
}

/// Decodes one hostile, length-bounded adapter frame.
///
/// The caller owns the outer length prefix and must never pass more than the
/// negotiated pipe ceiling.
///
/// # Errors
///
/// Returns [`AdapterContractError`] for an empty, oversized, malformed, or
/// message-less frame.
pub fn decode_adapter_frame(encoded: &[u8]) -> Result<AdapterFrame, AdapterContractError> {
    if encoded.is_empty() || encoded.len() > MAX_ADAPTER_FRAME_BYTES {
        return Err(AdapterContractError::FrameSize);
    }
    let frame = AdapterFrame::decode(encoded).map_err(|_| AdapterContractError::MalformedFrame)?;
    if frame.message.is_none() {
        return Err(AdapterContractError::MissingField);
    }
    Ok(frame)
}

/// Encodes one adapter frame after verifying that it has one message and fits
/// the fixed pipe ceiling.
///
/// # Errors
///
/// Returns [`AdapterContractError`] for a message-less or oversized frame.
pub fn encode_adapter_frame(frame: &AdapterFrame) -> Result<Vec<u8>, AdapterContractError> {
    if frame.message.is_none() {
        return Err(AdapterContractError::MissingField);
    }
    let encoded_len = frame.encoded_len();
    if encoded_len == 0 || encoded_len > MAX_ADAPTER_FRAME_BYTES {
        return Err(AdapterContractError::FrameSize);
    }
    let encoded = frame.encode_to_vec();
    Ok(encoded)
}

/// Returns the message family carried by a validated frame without exposing
/// repository-derived payloads.
#[must_use]
pub const fn frame_kind(frame: &AdapterFrame) -> &'static str {
    match frame.message {
        Some(adapter_frame::Message::Advertisement(_)) => "advertisement",
        Some(adapter_frame::Message::Session(_)) => "session",
        Some(adapter_frame::Message::AnalysisRequest(_)) => "analysis_request",
        Some(adapter_frame::Message::AnalysisResult(_)) => "analysis_result",
        Some(adapter_frame::Message::Cancel(_)) => "cancel",
        Some(adapter_frame::Message::Error(_)) => "error",
        None => "missing",
    }
}

/// Source-redacted adapter protocol contract failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AdapterContractError {
    /// A required protobuf message or oneof was absent.
    #[error("adapter protocol field is missing")]
    MissingField,
    /// Adapter binary identity labels or digest were invalid.
    #[error("adapter binary identity is invalid")]
    InvalidIdentity,
    /// The expected and advertised binary identities differ.
    #[error("adapter binary identity does not match")]
    IdentityMismatch,
    /// Host and adapter protocol ranges do not overlap.
    #[error("adapter protocol version is unsupported")]
    ProtocolMismatch,
    /// Capability labels were malformed, duplicated, or unavailable.
    #[error("adapter capability contract is invalid")]
    CapabilityMismatch,
    /// Extension declarations were malformed, duplicated, or unavailable.
    #[error("adapter extension contract is invalid")]
    ExtensionMismatch,
    /// Trust level was absent or unknown.
    #[error("adapter trust level is invalid")]
    InvalidTrust,
    /// Adapter trust exceeds the host policy.
    #[error("adapter trust exceeds host policy")]
    TrustMismatch,
    /// Resource limits were zero, excessive, or exceeded the child ceiling.
    #[error("adapter resource limits are invalid")]
    QuotaMismatch,
    /// The host requires cooperative cancellation.
    #[error("adapter cancellation capability is required")]
    CancellationRequired,
    /// Session or request correlation was malformed or stale.
    #[error("adapter session correlation is invalid")]
    SessionMismatch,
    /// Encoded frame was empty or exceeded the pipe ceiling.
    #[error("adapter frame size is invalid")]
    FrameSize,
    /// Protobuf frame bytes were malformed.
    #[error("adapter frame encoding is invalid")]
    MalformedFrame,
}

fn select_protocol(range: &VersionRange) -> Result<ContractVersion, AdapterContractError> {
    let minimum = range
        .minimum
        .ok_or(AdapterContractError::ProtocolMismatch)?;
    let maximum = range
        .maximum
        .ok_or(AdapterContractError::ProtocolMismatch)?;
    if minimum.major != ADAPTER_PROTOCOL_MAJOR
        || maximum.major != ADAPTER_PROTOCOL_MAJOR
        || minimum.minor > maximum.minor
        || maximum.minor < MINIMUM_ADAPTER_PROTOCOL_MINOR
    {
        return Err(AdapterContractError::ProtocolMismatch);
    }
    let selected_minor = maximum.minor.min(CURRENT_ADAPTER_PROTOCOL_MINOR);
    if selected_minor < minimum.minor {
        return Err(AdapterContractError::ProtocolMismatch);
    }
    Ok(ContractVersion {
        major: ADAPTER_PROTOCOL_MAJOR,
        minor: selected_minor,
    })
}

fn validate_identity(identity: &AdapterIdentity) -> Result<(), AdapterContractError> {
    validate_protocol_label(&identity.name).map_err(|_| AdapterContractError::InvalidIdentity)?;
    validate_version_label(&identity.version)?;
    validate_digest(&identity.source_digest).map_err(|_| AdapterContractError::InvalidIdentity)
}

fn validate_capabilities(
    capabilities: &[String],
) -> Result<BTreeSet<String>, AdapterContractError> {
    if capabilities.is_empty() || capabilities.len() > MAX_ADAPTER_DECLARATIONS {
        return Err(AdapterContractError::CapabilityMismatch);
    }
    let mut validated = BTreeSet::new();
    for capability in capabilities {
        validate_protocol_label(capability)
            .map_err(|_| AdapterContractError::CapabilityMismatch)?;
        if !validated.insert(capability.clone()) {
            return Err(AdapterContractError::CapabilityMismatch);
        }
    }
    Ok(validated)
}

fn validate_extensions(
    extensions: &[ExtensionDescriptor],
) -> Result<BTreeMap<String, ExtensionDescriptor>, AdapterContractError> {
    if extensions.len() > MAX_ADAPTER_DECLARATIONS {
        return Err(AdapterContractError::ExtensionMismatch);
    }
    let mut validated = BTreeMap::new();
    for extension in extensions {
        validate_extension_namespace(&extension.namespace)?;
        let version = extension
            .version
            .ok_or(AdapterContractError::ExtensionMismatch)?;
        if version.major == 0 {
            return Err(AdapterContractError::ExtensionMismatch);
        }
        if validated
            .insert(extension.namespace.clone(), extension.clone())
            .is_some()
        {
            return Err(AdapterContractError::ExtensionMismatch);
        }
    }
    Ok(validated)
}

fn validate_limits(limits: &ResourceLimits) -> Result<(), AdapterContractError> {
    if limits.wall_time_ms == 0
        || limits.wall_time_ms > MAX_WALL_TIME_MS
        || limits.cpu_time_ms == 0
        || limits.cpu_time_ms > MAX_CPU_TIME_MS
        || limits.memory_bytes == 0
        || limits.memory_bytes > MAX_MEMORY_BYTES
        || limits.input_bytes == 0
        || limits.input_bytes > MAX_INPUT_BYTES
        || limits.output_bytes == 0
        || limits.output_bytes > MAX_OUTPUT_BYTES
        || limits.files == 0
        || limits.files > MAX_FILES
        || limits.processes == 0
        || limits.processes > MAX_PROCESSES
        || limits.handles == 0
        || limits.handles > MAX_HANDLES
        || limits.retries > MAX_RETRIES
    {
        return Err(AdapterContractError::QuotaMismatch);
    }
    Ok(())
}

fn limits_fit(granted: &ResourceLimits, ceiling: &ResourceLimits) -> bool {
    granted.wall_time_ms <= ceiling.wall_time_ms
        && granted.cpu_time_ms <= ceiling.cpu_time_ms
        && granted.memory_bytes <= ceiling.memory_bytes
        && granted.input_bytes <= ceiling.input_bytes
        && granted.output_bytes <= ceiling.output_bytes
        && granted.files <= ceiling.files
        && granted.processes <= ceiling.processes
        && granted.handles <= ceiling.handles
        && granted.retries <= ceiling.retries
}

fn validate_protocol_label(label: &str) -> Result<(), AdapterContractError> {
    if label.is_empty()
        || label.len() > MAX_LABEL_BYTES
        || !label.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return Err(AdapterContractError::InvalidIdentity);
    }
    Ok(())
}

fn validate_version_label(label: &str) -> Result<(), AdapterContractError> {
    if label.is_empty()
        || label.len() > MAX_LABEL_BYTES
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(AdapterContractError::InvalidIdentity);
    }
    Ok(())
}

fn validate_extension_namespace(namespace: &str) -> Result<(), AdapterContractError> {
    validate_protocol_label(namespace).map_err(|_| AdapterContractError::ExtensionMismatch)?;
    if !namespace.contains('.') {
        return Err(AdapterContractError::ExtensionMismatch);
    }
    Ok(())
}

fn validate_nonce(value: &[u8]) -> Result<(), AdapterContractError> {
    validate_fixed_bytes(value, ADAPTER_NONCE_BYTES)
        .map_err(|_| AdapterContractError::SessionMismatch)?;
    if value.iter().all(|byte| *byte == 0) {
        return Err(AdapterContractError::SessionMismatch);
    }
    Ok(())
}

fn validate_digest(value: &[u8]) -> Result<(), AdapterContractError> {
    validate_fixed_bytes(value, ADAPTER_DIGEST_BYTES)
        .map_err(|_| AdapterContractError::InvalidIdentity)
}

fn validate_fixed_bytes(value: &[u8], expected: usize) -> Result<(), AdapterContractError> {
    if value.len() != expected {
        Err(AdapterContractError::InvalidIdentity)
    } else {
        Ok(())
    }
}

const fn trust_rank(trust: AdapterTrustLevel) -> u8 {
    match trust {
        AdapterTrustLevel::FirstParty => 1,
        AdapterTrustLevel::UserProvided => 2,
        AdapterTrustLevel::Untrusted => 3,
        AdapterTrustLevel::Unspecified => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::common::v1::{ContentHash, FileId, GenerationId, RepositoryId};

    #[test]
    fn negotiation_binds_identity_capabilities_extensions_quotas_and_cancellation() {
        let validated =
            ValidatedAdvertisement::validate(advertisement()).expect("advertisement validates");
        let session = validated
            .negotiate(requirements())
            .expect("host requirements negotiate");

        assert_eq!(session.session_id(), &[7; ADAPTER_NONCE_BYTES]);
        assert_eq!(session.adapter(), validated.identity());
        assert_eq!(session.protocol(), validated.selected_protocol());
        assert!(session.has_capability("semantic.types"));
        assert_eq!(session.trust_level(), AdapterTrustLevel::FirstParty);
    }

    #[test]
    fn negotiation_rejects_major_identity_quota_extension_and_cancellation_drift() {
        let mut legacy_minor = advertisement();
        legacy_minor.supported_protocols = Some(VersionRange {
            minimum: Some(ContractVersion { major: 1, minor: 0 }),
            maximum: Some(ContractVersion { major: 1, minor: 0 }),
        });
        assert_eq!(
            ValidatedAdvertisement::validate(legacy_minor),
            Err(AdapterContractError::ProtocolMismatch)
        );

        let mut major = advertisement();
        major.supported_protocols = Some(VersionRange {
            minimum: Some(ContractVersion { major: 2, minor: 0 }),
            maximum: Some(ContractVersion { major: 2, minor: 0 }),
        });
        assert_eq!(
            ValidatedAdvertisement::validate(major),
            Err(AdapterContractError::ProtocolMismatch)
        );

        let validated =
            ValidatedAdvertisement::validate(advertisement()).expect("advertisement validates");
        let mut identity = requirements();
        identity
            .expected_adapter
            .as_mut()
            .expect("identity exists")
            .source_digest[0] ^= 1;
        assert_eq!(
            validated.negotiate(identity),
            Err(AdapterContractError::IdentityMismatch)
        );

        let mut quota = requirements();
        quota
            .granted_limits
            .as_mut()
            .expect("limits exist")
            .memory_bytes *= 3;
        assert_eq!(
            validated.negotiate(quota),
            Err(AdapterContractError::QuotaMismatch)
        );

        let mut extension = requirements();
        extension.required_extensions[0]
            .version
            .as_mut()
            .expect("version exists")
            .minor = 2;
        assert_eq!(
            validated.negotiate(extension),
            Err(AdapterContractError::ExtensionMismatch)
        );

        let mut no_cancel = advertisement();
        no_cancel.supports_cancellation = false;
        assert_eq!(
            ValidatedAdvertisement::validate(no_cancel),
            Err(AdapterContractError::CancellationRequired)
        );
    }

    #[test]
    fn negotiated_session_bounds_requests_results_and_cancellation() {
        let session = ValidatedAdvertisement::validate(advertisement())
            .expect("advertisement validates")
            .negotiate(requirements())
            .expect("requirements negotiate");
        let request = analysis_request(32);
        session
            .validate_analysis_request(&request)
            .expect("bounded request validates");
        let result = AnalysisResult {
            session_id: vec![7; ADAPTER_NONCE_BYTES],
            request_id: vec![9; ADAPTER_NONCE_BYTES],
            normalized_ir: vec![1; 64],
            output_digest: Some(ContentHash {
                value: vec![3; ADAPTER_DIGEST_BYTES],
            }),
        };
        session
            .validate_analysis_result(&result)
            .expect("bounded result validates");
        session
            .validate_cancel(&CancelRequest {
                session_id: vec![7; ADAPTER_NONCE_BYTES],
                request_id: vec![9; ADAPTER_NONCE_BYTES],
            })
            .expect("correlated cancellation validates");

        let mut oversized = analysis_request(1_025);
        assert_eq!(
            session.validate_analysis_request(&oversized),
            Err(AdapterContractError::QuotaMismatch)
        );
        oversized.session_id[0] ^= 1;
        assert_eq!(
            session.validate_analysis_request(&oversized),
            Err(AdapterContractError::SessionMismatch)
        );

        let mut default_nonce = analysis_request(32);
        default_nonce.request_id.fill(0);
        assert_eq!(
            session.validate_analysis_request(&default_nonce),
            Err(AdapterContractError::SessionMismatch)
        );
    }

    #[test]
    fn frame_decoder_rejects_missing_malformed_and_oversized_messages() {
        assert_eq!(
            decode_adapter_frame(&[]),
            Err(AdapterContractError::FrameSize)
        );
        assert_eq!(
            decode_adapter_frame(&[0xff]),
            Err(AdapterContractError::MalformedFrame)
        );
        assert_eq!(
            decode_adapter_frame(&AdapterFrame { message: None }.encode_to_vec()),
            Err(AdapterContractError::FrameSize)
        );
        assert_eq!(
            decode_adapter_frame(&vec![0; MAX_ADAPTER_FRAME_BYTES + 1]),
            Err(AdapterContractError::FrameSize)
        );

        let frame = AdapterFrame {
            message: Some(adapter_frame::Message::Cancel(CancelRequest {
                session_id: vec![7; ADAPTER_NONCE_BYTES],
                request_id: vec![9; ADAPTER_NONCE_BYTES],
            })),
        };
        let encoded = encode_adapter_frame(&frame).expect("frame encodes");
        let decoded = decode_adapter_frame(&encoded).expect("frame decodes");
        assert_eq!(frame_kind(&decoded), "cancel");

        let oversized_frame = AdapterFrame {
            message: Some(adapter_frame::Message::AnalysisResult(AnalysisResult {
                session_id: vec![7; ADAPTER_NONCE_BYTES],
                request_id: vec![9; ADAPTER_NONCE_BYTES],
                normalized_ir: vec![0; MAX_ADAPTER_FRAME_BYTES],
                output_digest: Some(ContentHash {
                    value: vec![3; ADAPTER_DIGEST_BYTES],
                }),
            })),
        };
        assert_eq!(
            encode_adapter_frame(&oversized_frame),
            Err(AdapterContractError::FrameSize)
        );
    }

    fn advertisement() -> CapabilityAdvertisement {
        CapabilityAdvertisement {
            adapter: Some(identity()),
            supported_protocols: Some(VersionRange {
                minimum: Some(ContractVersion {
                    major: ADAPTER_PROTOCOL_MAJOR,
                    minor: MINIMUM_ADAPTER_PROTOCOL_MINOR,
                }),
                maximum: Some(ContractVersion {
                    major: ADAPTER_PROTOCOL_MAJOR,
                    minor: CURRENT_ADAPTER_PROTOCOL_MINOR,
                }),
            }),
            capabilities: vec!["semantic.scopes".to_owned(), "semantic.types".to_owned()],
            extensions: vec![extension()],
            trust_level: AdapterTrustLevel::FirstParty as i32,
            hard_limits: Some(limits(2_048)),
            supports_cancellation: true,
        }
    }

    fn requirements() -> SessionRequirements {
        SessionRequirements {
            session_id: vec![7; ADAPTER_NONCE_BYTES],
            selected_protocol: Some(ContractVersion {
                major: ADAPTER_PROTOCOL_MAJOR,
                minor: CURRENT_ADAPTER_PROTOCOL_MINOR,
            }),
            expected_adapter: Some(identity()),
            required_capabilities: vec!["semantic.types".to_owned()],
            required_extensions: vec![extension()],
            granted_limits: Some(limits(1_024)),
            maximum_trust: AdapterTrustLevel::FirstParty as i32,
            require_cancellation: true,
        }
    }

    fn identity() -> AdapterIdentity {
        AdapterIdentity {
            name: "rootlight-adapters".to_owned(),
            version: "0.1.0".to_owned(),
            source_digest: vec![1; ADAPTER_DIGEST_BYTES],
        }
    }

    fn extension() -> ExtensionDescriptor {
        ExtensionDescriptor {
            namespace: "rootlight.types".to_owned(),
            version: Some(ContractVersion { major: 1, minor: 0 }),
            critical: true,
        }
    }

    fn limits(memory_bytes: u64) -> ResourceLimits {
        ResourceLimits {
            wall_time_ms: 5_000,
            cpu_time_ms: 4_000,
            memory_bytes,
            input_bytes: 1_024,
            output_bytes: 1_024,
            files: 4,
            processes: 1,
            handles: 16,
            retries: 0,
        }
    }

    fn analysis_request(source_bytes: usize) -> AnalysisRequest {
        AnalysisRequest {
            session_id: vec![7; ADAPTER_NONCE_BYTES],
            request_id: vec![9; ADAPTER_NONCE_BYTES],
            repository: Some(RepositoryId { value: vec![1; 16] }),
            generation: Some(GenerationId { value: vec![2; 20] }),
            file: Some(FileId { value: vec![3; 20] }),
            language: "rust".to_owned(),
            build_context: Some(ContentHash {
                value: vec![4; ADAPTER_DIGEST_BYTES],
            }),
            source_digest: Some(ContentHash {
                value: vec![5; ADAPTER_DIGEST_BYTES],
            }),
            source: vec![6; source_bytes],
        }
    }
}
