//! Stable, bounded, source-redacted errors for Rootlight's public boundaries.
//!
//! Internal crates retain typed causal errors. This crate owns only the safe
//! envelope that may cross CLI, IPC, MCP, adapter, and storage boundaries.

#![forbid(unsafe_code)]

use std::{collections::BTreeMap, fmt, time::Duration};

use rootlight_ids::{GenerationId, OperationId, RepositoryId};
use serde::{Deserialize, Serialize};

const MAX_MESSAGE_BYTES: usize = 1_024;
const MAX_DETAILS: usize = 32;
const MAX_DETAIL_KEY_BYTES: usize = 64;
const MAX_NEXT_ACTIONS: usize = 8;
const MAX_RETRY_AFTER_MS: u64 = 24 * 60 * 60 * 1_000;
const MAX_RETRY_AFTER: Duration = Duration::from_millis(MAX_RETRY_AFTER_MS);

/// Version of the stable public error registry.
pub const ERROR_REGISTRY_VERSION: &str = "1.0";

const fn is_safe_message_template(message: &str) -> bool {
    let bytes = message.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_MESSAGE_BYTES {
        return false;
    }
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b' ' | b'-')) {
            return false;
        }
        index += 1;
    }
    true
}

/// Stable public error families shared by all Rootlight boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum ErrorCode {
    /// The caller supplied an invalid value.
    InvalidArgument,
    /// The requested entity does not exist.
    NotFound,
    /// The request conflicts with current state.
    Conflict,
    /// The selected generation is no longer valid for this operation.
    StaleGeneration,
    /// The requested capability is unavailable.
    UnsupportedCapability,
    /// The result is incomplete for the requested coverage.
    IncompleteCoverage,
    /// The operation exceeded an explicit work budget.
    BudgetExceeded,
    /// A bounded resource is exhausted.
    ResourceExhausted,
    /// The operation was cancelled before completion.
    Cancelled,
    /// An isolated adapter failed.
    AdapterFailed,
    /// An index failed integrity checks.
    IndexCorrupt,
    /// Stored data requires a supported migration.
    MigrationRequired,
    /// Policy denied the requested operation.
    PermissionDenied,
    /// A protocol or contract version is incompatible.
    ProtocolMismatch,
    /// A conflicting operation temporarily owns the resource.
    Busy,
    /// An internal failure cannot be safely disclosed.
    Internal,
    /// A pagination cursor is invalid, expired, forged, or context-mismatched.
    InvalidCursor,
    /// A supplied value has the wrong type for its target field.
    TypeMismatch,
    /// The request exceeded a cost limit before execution.
    CostLimit,
    /// The query uses an operator outside the documented allowlist.
    OperatorForbidden,
    /// A batch binding reference is malformed or unresolved.
    BindingInvalid,
    /// A batch binding produced a value of the wrong type for its target.
    BindingTypeMismatch,
}

impl ErrorCode {
    /// Every stable public error code in wire-number order.
    pub const ALL: [Self; 22] = [
        Self::InvalidArgument,
        Self::NotFound,
        Self::Conflict,
        Self::StaleGeneration,
        Self::UnsupportedCapability,
        Self::IncompleteCoverage,
        Self::BudgetExceeded,
        Self::ResourceExhausted,
        Self::Cancelled,
        Self::AdapterFailed,
        Self::IndexCorrupt,
        Self::MigrationRequired,
        Self::PermissionDenied,
        Self::ProtocolMismatch,
        Self::Busy,
        Self::Internal,
        Self::InvalidCursor,
        Self::TypeMismatch,
        Self::CostLimit,
        Self::OperatorForbidden,
        Self::BindingInvalid,
        Self::BindingTypeMismatch,
    ];

    /// Returns the stable protobuf numeric representation.
    #[must_use]
    pub const fn wire_number(self) -> i32 {
        match self {
            Self::InvalidArgument => 1,
            Self::NotFound => 2,
            Self::Conflict => 3,
            Self::StaleGeneration => 4,
            Self::UnsupportedCapability => 5,
            Self::IncompleteCoverage => 6,
            Self::BudgetExceeded => 7,
            Self::ResourceExhausted => 8,
            Self::Cancelled => 9,
            Self::AdapterFailed => 10,
            Self::IndexCorrupt => 11,
            Self::MigrationRequired => 12,
            Self::PermissionDenied => 13,
            Self::ProtocolMismatch => 14,
            Self::Busy => 15,
            Self::Internal => 16,
            Self::InvalidCursor => 17,
            Self::TypeMismatch => 18,
            Self::CostLimit => 19,
            Self::OperatorForbidden => 20,
            Self::BindingInvalid => 21,
            Self::BindingTypeMismatch => 22,
        }
    }

    /// Decodes one stable protobuf numeric representation.
    #[must_use]
    pub const fn from_wire_number(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::InvalidArgument),
            2 => Some(Self::NotFound),
            3 => Some(Self::Conflict),
            4 => Some(Self::StaleGeneration),
            5 => Some(Self::UnsupportedCapability),
            6 => Some(Self::IncompleteCoverage),
            7 => Some(Self::BudgetExceeded),
            8 => Some(Self::ResourceExhausted),
            9 => Some(Self::Cancelled),
            10 => Some(Self::AdapterFailed),
            11 => Some(Self::IndexCorrupt),
            12 => Some(Self::MigrationRequired),
            13 => Some(Self::PermissionDenied),
            14 => Some(Self::ProtocolMismatch),
            15 => Some(Self::Busy),
            16 => Some(Self::Internal),
            17 => Some(Self::InvalidCursor),
            18 => Some(Self::TypeMismatch),
            19 => Some(Self::CostLimit),
            20 => Some(Self::OperatorForbidden),
            21 => Some(Self::BindingInvalid),
            22 => Some(Self::BindingTypeMismatch),
            _ => None,
        }
    }
}

/// Stable recommended remediation class for a public error code.
///
/// The class is derived from the code alone so client automation can react
/// deterministically without parsing free-text messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remediation {
    /// No client action repairs this request.
    None,
    /// Retry the identical request, optionally after the envelope delay.
    Retry,
    /// Correct one or more client-supplied input fields.
    CorrectInput,
    /// Restart the enumeration or query from the beginning.
    RestartEnumeration,
    /// Reduce the requested scope, limit, or budget.
    ReduceScope,
    /// Select a supported contract version.
    SelectSupportedVersion,
    /// Rebuild the affected repository generation.
    RebuildRepository,
    /// Collect a source-free support bundle.
    CollectSupportBundle,
}

/// One normative entry in the stable public error registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorDefinition {
    /// Stable domain code.
    pub code: ErrorCode,
    /// Stable uppercase wire name.
    pub name: &'static str,
    /// Stable protobuf numeric value.
    pub wire_number: i32,
    /// Default source-free message.
    pub message: &'static str,
    /// Whether the unchanged request may be retried.
    pub retryable: bool,
    /// Recommended client remediation class.
    pub remediation: Remediation,
}

macro_rules! error_registry {
    ($(($code:ident, $name:literal, $wire:literal, $message:literal, $retryable:literal, $remediation:ident)),+ $(,)?) => {
        /// Normative stable public error registry in protobuf numeric order.
        pub const ERROR_REGISTRY: [ErrorDefinition; 22] = [
            $(ErrorDefinition {
                code: ErrorCode::$code,
                name: $name,
                wire_number: $wire,
                message: $message,
                retryable: $retryable,
                remediation: Remediation::$remediation,
            }),+
        ];

        /// Returns the normative registry entry for a public code.
        #[must_use]
        pub const fn error_definition(code: ErrorCode) -> &'static ErrorDefinition {
            match code {
                $(ErrorCode::$code => &ERROR_REGISTRY[$wire - 1]),+
            }
        }
    };
}

error_registry!(
    (
        InvalidArgument,
        "INVALID_ARGUMENT",
        1,
        "tool arguments are invalid",
        false,
        CorrectInput
    ),
    (
        NotFound,
        "NOT_FOUND",
        2,
        "requested entity was not found",
        false,
        None
    ),
    (
        Conflict,
        "CONFLICT",
        3,
        "request conflicts with current state",
        true,
        Retry
    ),
    (
        StaleGeneration,
        "STALE_GENERATION",
        4,
        "selected generation is stale",
        false,
        RestartEnumeration
    ),
    (
        UnsupportedCapability,
        "UNSUPPORTED_CAPABILITY",
        5,
        "requested capability is unavailable",
        false,
        ReduceScope
    ),
    (
        IncompleteCoverage,
        "INCOMPLETE_COVERAGE",
        6,
        "requested data is unavailable",
        false,
        ReduceScope
    ),
    (
        BudgetExceeded,
        "BUDGET_EXCEEDED",
        7,
        "request budget is exhausted",
        false,
        ReduceScope
    ),
    (
        ResourceExhausted,
        "RESOURCE_EXHAUSTED",
        8,
        "bounded resource is exhausted",
        true,
        Retry
    ),
    (
        Cancelled,
        "CANCELLED",
        9,
        "operation was cancelled",
        true,
        Retry
    ),
    (
        AdapterFailed,
        "ADAPTER_FAILED",
        10,
        "adapter operation failed",
        true,
        Retry
    ),
    (
        IndexCorrupt,
        "INDEX_CORRUPT",
        11,
        "repository index is corrupt",
        false,
        RebuildRepository
    ),
    (
        MigrationRequired,
        "MIGRATION_REQUIRED",
        12,
        "stored data requires migration",
        false,
        RebuildRepository
    ),
    (
        PermissionDenied,
        "PERMISSION_DENIED",
        13,
        "request is denied by policy",
        false,
        None
    ),
    (
        ProtocolMismatch,
        "PROTOCOL_MISMATCH",
        14,
        "protocol version is incompatible",
        false,
        SelectSupportedVersion
    ),
    (Busy, "BUSY", 15, "requested resource is busy", true, Retry),
    (
        Internal,
        "INTERNAL",
        16,
        "internal operation failed",
        false,
        CollectSupportBundle
    ),
    (
        InvalidCursor,
        "INVALID_CURSOR",
        17,
        "pagination cursor is invalid or expired",
        false,
        RestartEnumeration
    ),
    (
        TypeMismatch,
        "TYPE_MISMATCH",
        18,
        "tool argument type does not match",
        false,
        CorrectInput
    ),
    (
        CostLimit,
        "COST_LIMIT",
        19,
        "query cost limit was exceeded",
        false,
        ReduceScope
    ),
    (
        OperatorForbidden,
        "OPERATOR_FORBIDDEN",
        20,
        "query operator is forbidden",
        false,
        CorrectInput
    ),
    (
        BindingInvalid,
        "BINDING_INVALID",
        21,
        "batch binding is invalid",
        false,
        CorrectInput
    ),
    (
        BindingTypeMismatch,
        "BINDING_TYPE_MISMATCH",
        22,
        "batch binding type does not match",
        false,
        CorrectInput
    ),
);

/// Reports whether a request that failed with this code is safe to retry
/// unchanged.
#[must_use]
pub const fn error_retryable(code: ErrorCode) -> bool {
    error_definition(code).retryable
}

/// Returns the stable recommended remediation class for a public error code.
#[must_use]
pub const fn error_remediation(code: ErrorCode) -> Remediation {
    error_definition(code).remediation
}

/// A validated key for a bounded public error detail.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct DetailKey(
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 64), regex(pattern = r"^[a-z0-9_]+$"))
    )]
    String,
);

impl DetailKey {
    /// Parses a key containing only lowercase ASCII letters, digits, and `_`.
    ///
    /// # Errors
    ///
    /// Returns [`PublicErrorBuildError::InvalidDetailKey`] when the key is
    /// empty, too long, or uses characters outside the safe allow-list.
    pub fn parse(value: &str) -> Result<Self, PublicErrorBuildError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_DETAIL_KEY_BYTES
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if !valid {
            return Err(PublicErrorBuildError::InvalidDetailKey);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the validated key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DetailKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("DetailKey").field(&self.0).finish()
    }
}

impl<'de> Deserialize<'de> for DetailKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// A short source-free label permitted in public diagnostics.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct SafeLabel(
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 128), regex(pattern = r"^[A-Za-z0-9_.:-]+$"))
    )]
    String,
);

impl SafeLabel {
    /// Parses an ASCII label that cannot contain paths, whitespace, or controls.
    ///
    /// # Errors
    ///
    /// Returns [`PublicErrorBuildError::InvalidSafeLabel`] for unsafe or
    /// oversized labels.
    pub fn parse(value: &str) -> Result<Self, PublicErrorBuildError> {
        let valid = !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
            });
        if !valid {
            return Err(PublicErrorBuildError::InvalidSafeLabel);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the validated label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SafeLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("SafeLabel").field(&self.0).finish()
    }
}

impl<'de> Deserialize<'de> for SafeLabel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Bounded primitive values permitted in public error details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    deny_unknown_fields,
    tag = "type",
    content = "value",
    rename_all = "snake_case"
)]
#[non_exhaustive]
pub enum PublicValue {
    /// A boolean property.
    Boolean(bool),
    /// A signed integer property.
    Integer(i64),
    /// An unsigned integer property.
    Unsigned(u64),
    /// A repository identity.
    Repository(RepositoryId),
    /// A generation identity.
    Generation(GenerationId),
    /// An operation identity.
    Operation(OperationId),
    /// A validated source-free label.
    Label(SafeLabel),
}

/// Stable remediation hints generated from a closed set of templates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, tag = "action", rename_all = "snake_case")]
#[non_exhaustive]
pub enum NextAction {
    /// Correct one named input field.
    CorrectField {
        /// Stable name of the invalid field.
        field: DetailKey,
    },
    /// Retry after the bounded delay in the envelope.
    Retry,
    /// Select a compatible contract version.
    SelectSupportedVersion,
    /// Inspect the associated operation status.
    InspectOperation,
    /// Rebuild the affected repository generation.
    RebuildRepository,
    /// Collect a protected source-free support bundle.
    CollectSupportBundle,
    /// Restart the enumeration or query from the beginning after an invalid
    /// continuation cursor.
    RestartEnumeration,
}

/// A stable source-redacted failure safe to serialize across public boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PublicError {
    code: ErrorCode,
    #[cfg_attr(feature = "schema", schemars(length(max = 1_024)))]
    message: String,
    retryable: bool,
    #[cfg_attr(feature = "schema", schemars(range(max = 86_400_000)))]
    retry_after_ms: Option<u64>,
    repository: Option<RepositoryId>,
    operation: Option<OperationId>,
    generation: Option<GenerationId>,
    #[cfg_attr(feature = "schema", schemars(length(max = 32)))]
    details: BTreeMap<DetailKey, PublicValue>,
    #[cfg_attr(feature = "schema", schemars(length(max = 8)))]
    next_actions: Vec<NextAction>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicErrorWire {
    code: ErrorCode,
    message: String,
    retryable: bool,
    retry_after_ms: Option<u64>,
    repository: Option<RepositoryId>,
    operation: Option<OperationId>,
    generation: Option<GenerationId>,
    details: BTreeMap<DetailKey, PublicValue>,
    next_actions: Vec<NextAction>,
}

impl<'de> Deserialize<'de> for PublicError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = PublicErrorWire::deserialize(deserializer)?;
        if !is_safe_message_template(&wire.message) {
            return Err(serde::de::Error::custom(
                "public error message violates the safe template policy",
            ));
        }
        if wire.details.len() > MAX_DETAILS {
            return Err(serde::de::Error::custom(
                "public error has too many details",
            ));
        }
        if wire.next_actions.len() > MAX_NEXT_ACTIONS {
            return Err(serde::de::Error::custom(
                "public error has too many next actions",
            ));
        }
        if wire
            .retry_after_ms
            .is_some_and(|delay| delay > MAX_RETRY_AFTER_MS)
        {
            return Err(serde::de::Error::custom(
                "public error retry delay exceeds its limit",
            ));
        }
        if wire.retry_after_ms.is_some() && !wire.retryable {
            return Err(serde::de::Error::custom(
                "public error retry delay requires retryable state",
            ));
        }

        Ok(Self {
            code: wire.code,
            message: wire.message,
            retryable: wire.retryable,
            retry_after_ms: wire.retry_after_ms,
            repository: wire.repository,
            operation: wire.operation,
            generation: wire.generation,
            details: wire.details,
            next_actions: wire.next_actions,
        })
    }
}

impl PublicError {
    /// Starts a checked public error using a static source-free message template.
    pub fn builder(code: ErrorCode, message: &'static str) -> PublicErrorBuilder {
        PublicErrorBuilder::new(code, message.to_owned())
    }

    /// Starts a checked public error from an owned boundary message.
    ///
    /// This is intended for trusted protocol decoders that must preserve the
    /// sender's stable message while reapplying Rootlight's source-free policy.
    pub fn builder_with_message(code: ErrorCode, message: String) -> PublicErrorBuilder {
        PublicErrorBuilder::new(code, message)
    }

    /// Returns the stable error family.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// Returns the source-free display template.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Reports whether an unchanged request may succeed when retried.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        self.retryable
    }

    /// Returns the optional bounded retry delay.
    #[must_use]
    pub const fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }

    /// Returns the associated repository identity, when present.
    #[must_use]
    pub const fn repository(&self) -> Option<RepositoryId> {
        self.repository
    }

    /// Returns the associated operation identity, when present.
    #[must_use]
    pub const fn operation(&self) -> Option<OperationId> {
        self.operation
    }

    /// Returns the associated generation identity, when present.
    #[must_use]
    pub const fn generation(&self) -> Option<GenerationId> {
        self.generation
    }

    /// Returns the bounded structured details.
    #[must_use]
    pub const fn details(&self) -> &BTreeMap<DetailKey, PublicValue> {
        &self.details
    }

    /// Returns the bounded remediation hints.
    #[must_use]
    pub fn next_actions(&self) -> &[NextAction] {
        &self.next_actions
    }
}

/// Checked construction for a bounded [`PublicError`].
#[derive(Debug)]
#[must_use = "call build() to validate and create the public error"]
pub struct PublicErrorBuilder {
    error: PublicError,
    build_error: Option<PublicErrorBuildError>,
}

impl PublicErrorBuilder {
    fn new(code: ErrorCode, message: String) -> Self {
        let build_error = (!is_safe_message_template(&message))
            .then_some(PublicErrorBuildError::InvalidMessageTemplate);
        Self {
            error: PublicError {
                code,
                message,
                retryable: false,
                retry_after_ms: None,
                repository: None,
                operation: None,
                generation: None,
                details: BTreeMap::new(),
                next_actions: Vec::new(),
            },
            build_error,
        }
    }

    /// Marks the error retryable without specifying a delay.
    pub const fn retryable(mut self) -> Self {
        self.error.retryable = true;
        self
    }

    /// Marks the error retryable after a bounded delay.
    pub fn retry_after(mut self, delay: Duration) -> Self {
        if delay > MAX_RETRY_AFTER {
            self.build_error = Some(PublicErrorBuildError::RetryDelayTooLong);
        } else {
            let millis = delay.as_millis();
            match u64::try_from(millis) {
                Ok(millis) => {
                    self.error.retryable = true;
                    self.error.retry_after_ms = Some(millis);
                }
                Err(_) => self.build_error = Some(PublicErrorBuildError::RetryDelayTooLong),
            }
        }
        self
    }

    /// Associates a repository identity.
    pub const fn repository(mut self, repository: RepositoryId) -> Self {
        self.error.repository = Some(repository);
        self
    }

    /// Associates an operation identity.
    pub const fn operation(mut self, operation: OperationId) -> Self {
        self.error.operation = Some(operation);
        self
    }

    /// Associates a generation identity.
    pub const fn generation(mut self, generation: GenerationId) -> Self {
        self.error.generation = Some(generation);
        self
    }

    /// Adds a bounded typed detail.
    pub fn detail(mut self, key: DetailKey, value: PublicValue) -> Self {
        if self.error.details.len() >= MAX_DETAILS && !self.error.details.contains_key(&key) {
            self.build_error = Some(PublicErrorBuildError::TooManyDetails);
        } else {
            self.error.details.insert(key, value);
        }
        self
    }

    /// Adds a remediation hint from the closed public action set.
    pub fn next_action(mut self, action: NextAction) -> Self {
        if self.error.next_actions.len() >= MAX_NEXT_ACTIONS {
            self.build_error = Some(PublicErrorBuildError::TooManyNextActions);
        } else {
            self.error.next_actions.push(action);
        }
        self
    }

    /// Validates and creates the bounded public error.
    ///
    /// # Errors
    ///
    /// Returns the first construction error encountered by the builder.
    pub fn build(self) -> Result<PublicError, PublicErrorBuildError> {
        match self.build_error {
            Some(error) => Err(error),
            None => Ok(self.error),
        }
    }
}

/// Failures encountered while constructing safe public diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PublicErrorBuildError {
    /// The static message is empty, oversized, or outside the safe template alphabet.
    #[error("public error message violates the safe template policy")]
    InvalidMessageTemplate,
    /// A detail key violates the public allow-list.
    #[error("invalid public error detail key")]
    InvalidDetailKey,
    /// A label violates the public allow-list.
    #[error("invalid public error safe label")]
    InvalidSafeLabel,
    /// The detail map exceeds its item bound.
    #[error("public error has too many details")]
    TooManyDetails,
    /// The remediation list exceeds its item bound.
    #[error("public error has too many next actions")]
    TooManyNextActions,
    /// The retry delay exceeds 24 hours or cannot fit the wire integer.
    #[error("public error retry delay exceeds its limit")]
    RetryDelayTooLong,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::derive_repository;

    #[test]
    fn serializes_stable_machine_semantics() {
        let repository = derive_repository(b"repository fixture").id();
        let error = PublicError::builder(ErrorCode::NotFound, "repository was not found")
            .repository(repository)
            .next_action(NextAction::CorrectField {
                field: DetailKey::parse("repository_id").expect("hard-coded key is valid"),
            })
            .build()
            .expect("bounded fixture builds");

        let json = serde_json::to_string(&error).expect("public error serializes");
        assert!(json.contains("NOT_FOUND"));
        assert!(json.contains("correct_field"));
        assert!(!json.contains("repository fixture"));
    }

    #[test]
    fn rejects_path_shaped_and_secret_shaped_labels() {
        for unsafe_label in [
            r"C:\\Users\\person\\secret.rs",
            "/home/person/secret.rs",
            "token value",
            "line\nbreak",
        ] {
            assert_eq!(
                SafeLabel::parse(unsafe_label),
                Err(PublicErrorBuildError::InvalidSafeLabel)
            );
        }
    }

    #[test]
    fn deserialization_preserves_detail_key_invariants() {
        assert!(serde_json::from_str::<DetailKey>(r#""repository_id""#).is_ok());
        assert!(serde_json::from_str::<DetailKey>(r#""Repository/Path""#).is_err());
        assert!(
            serde_json::from_str::<NextAction>(
                r#"{"action":"correct_field","field":"Repository/Path"}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn deserialization_preserves_safe_label_invariants() {
        assert!(serde_json::from_str::<SafeLabel>(r#""adapter:v1""#).is_ok());
        assert!(serde_json::from_str::<SafeLabel>(r#""/home/person/secret.rs""#).is_err());
        assert!(
            serde_json::from_str::<PublicValue>(
                r#"{"type":"label","value":"/home/person/secret.rs"}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn retry_delay_is_bounded() {
        let result = PublicError::builder(ErrorCode::Busy, "repository is busy")
            .retry_after(Duration::from_secs(24 * 60 * 60 + 1))
            .build();
        assert_eq!(result, Err(PublicErrorBuildError::RetryDelayTooLong));
    }

    #[test]
    fn public_error_round_trips_through_checked_deserialization() {
        let expected = PublicError::builder(ErrorCode::Busy, "repository is busy")
            .retry_after(Duration::from_millis(250))
            .build()
            .expect("bounded fixture builds");
        let json = serde_json::to_string(&expected).expect("public error serializes");
        let actual: PublicError = serde_json::from_str(&json).expect("public error deserializes");

        assert_eq!(actual, expected);
    }

    #[test]
    fn public_error_deserialization_rejects_unchecked_bounds() {
        let oversized_message = "x".repeat(MAX_MESSAGE_BYTES + 1);
        let json = format!(
            r#"{{"code":"INTERNAL","message":"{oversized_message}","retryable":false,"retry_after_ms":null,"repository":null,"operation":null,"generation":null,"details":{{}},"next_actions":[]}}"#
        );
        assert!(serde_json::from_str::<PublicError>(&json).is_err());

        let inconsistent_retry = r#"{"code":"BUSY","message":"repository is busy","retryable":false,"retry_after_ms":1,"repository":null,"operation":null,"generation":null,"details":{},"next_actions":[]}"#;
        assert!(serde_json::from_str::<PublicError>(inconsistent_retry).is_err());

        let path_shaped_message = r#"{"code":"INTERNAL","message":"C:\\Users\\person\\secret.rs","retryable":false,"retry_after_ms":null,"repository":null,"operation":null,"generation":null,"details":{},"next_actions":[]}"#;
        assert!(serde_json::from_str::<PublicError>(path_shaped_message).is_err());
    }

    #[test]
    fn construction_rejects_source_shaped_message_templates() {
        assert_eq!(
            PublicError::builder(ErrorCode::Internal, "/home/person/secret.rs").build(),
            Err(PublicErrorBuildError::InvalidMessageTemplate)
        );
        assert_eq!(
            PublicError::builder_with_message(
                ErrorCode::Internal,
                "token=gho_example_secret".to_owned(),
            )
            .build(),
            Err(PublicErrorBuildError::InvalidMessageTemplate)
        );
    }

    #[test]
    fn owned_message_builder_preserves_checked_boundary_text() {
        let error = PublicError::builder_with_message(
            ErrorCode::ProtocolMismatch,
            "client protocol range is missing".to_owned(),
        )
        .build()
        .expect("bounded boundary message builds");

        assert_eq!(error.message(), "client protocol range is missing");
    }

    #[test]
    fn debug_and_serialization_do_not_contain_seeded_secrets() {
        let error = PublicError::builder(ErrorCode::Internal, "internal operation failed")
            .build()
            .expect("bounded fixture builds");
        let debug = format!("{error:?}");
        let json = serde_json::to_string(&error).expect("public error serializes");

        for forbidden in [
            "gho_example_secret",
            "BEGIN PRIVATE KEY",
            "C:\\Users\\person",
        ] {
            assert!(!debug.contains(forbidden));
            assert!(!json.contains(forbidden));
        }
    }

    #[test]
    fn new_error_codes_serialize_to_stable_names() {
        let cases = [
            (ErrorCode::InvalidCursor, "\"INVALID_CURSOR\""),
            (ErrorCode::TypeMismatch, "\"TYPE_MISMATCH\""),
            (ErrorCode::CostLimit, "\"COST_LIMIT\""),
            (ErrorCode::OperatorForbidden, "\"OPERATOR_FORBIDDEN\""),
            (ErrorCode::BindingInvalid, "\"BINDING_INVALID\""),
            (ErrorCode::BindingTypeMismatch, "\"BINDING_TYPE_MISMATCH\""),
        ];
        for (code, expected) in cases {
            assert_eq!(serde_json::to_string(&code).expect("serializes"), expected);
            let parsed: ErrorCode = serde_json::from_str(expected).expect("deserializes");
            assert_eq!(parsed, code, "round trip for {expected}");
        }
    }

    #[test]
    fn remediation_registry_classifies_the_new_codes() {
        assert_eq!(
            error_remediation(ErrorCode::InvalidCursor),
            Remediation::RestartEnumeration
        );
        assert_eq!(
            error_remediation(ErrorCode::TypeMismatch),
            Remediation::CorrectInput
        );
        assert_eq!(
            error_remediation(ErrorCode::BindingInvalid),
            Remediation::CorrectInput
        );
        assert_eq!(
            error_remediation(ErrorCode::BindingTypeMismatch),
            Remediation::CorrectInput
        );
        assert_eq!(
            error_remediation(ErrorCode::OperatorForbidden),
            Remediation::CorrectInput
        );
        assert_eq!(
            error_remediation(ErrorCode::CostLimit),
            Remediation::ReduceScope
        );
        assert!(!error_retryable(ErrorCode::InvalidCursor));
        assert!(!error_retryable(ErrorCode::TypeMismatch));
        assert!(!error_retryable(ErrorCode::CostLimit));
    }

    #[test]
    fn normative_registry_is_complete_and_wire_stable() {
        assert_eq!(ERROR_REGISTRY_VERSION, "1.0");
        assert_eq!(ERROR_REGISTRY.len(), ErrorCode::ALL.len());
        for (index, code) in ErrorCode::ALL.into_iter().enumerate() {
            let definition = error_definition(code);
            assert_eq!(definition.code, code);
            assert_eq!(
                usize::try_from(definition.wire_number).expect("wire number is positive"),
                index + 1
            );
            assert_eq!(
                ErrorCode::from_wire_number(definition.wire_number),
                Some(code)
            );
            assert_eq!(error_retryable(code), definition.retryable);
            assert_eq!(error_remediation(code), definition.remediation);
            assert!(is_safe_message_template(definition.message));
            assert_eq!(
                serde_json::to_value(code).expect("code serializes"),
                serde_json::Value::String(definition.name.to_owned())
            );
        }
        assert_eq!(ErrorCode::from_wire_number(0), None);
        assert_eq!(ErrorCode::from_wire_number(23), None);
    }

    #[test]
    fn versioned_registry_artifact_matches_the_normative_table() {
        let artifact: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/errors/error-registry-1.0.json"
        ))
        .expect("checked registry artifact is valid JSON");
        assert_eq!(artifact["schema_version"], ERROR_REGISTRY_VERSION);
        assert_eq!(artifact["compatibility"]["details"], "additive");
        assert_eq!(artifact["compatibility"]["unknown_major"], "reject");
        let entries = artifact["errors"]
            .as_array()
            .expect("checked registry has an errors array");
        assert_eq!(entries.len(), ERROR_REGISTRY.len());
        for (entry, definition) in entries.iter().zip(ERROR_REGISTRY) {
            assert_eq!(entry["code"], definition.name);
            assert_eq!(entry["wire_number"], definition.wire_number);
            assert_eq!(entry["message"], definition.message);
            assert_eq!(entry["retryable"], definition.retryable);
            assert_eq!(
                entry["remediation"],
                format!("{:?}", definition.remediation)
            );
        }
    }

    #[test]
    fn message_bound_accepts_the_limit_and_rejects_one_byte_more() {
        let maximum = "x".repeat(MAX_MESSAGE_BYTES);
        let accepted = PublicError::builder_with_message(ErrorCode::Internal, maximum).build();
        assert!(accepted.is_ok());

        let oversized = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert_eq!(
            PublicError::builder_with_message(ErrorCode::Internal, oversized).build(),
            Err(PublicErrorBuildError::InvalidMessageTemplate)
        );
    }
}
