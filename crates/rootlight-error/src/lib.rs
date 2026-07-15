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
const MAX_RETRY_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

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
}

/// A validated key for a bounded public error detail.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

/// A short source-free label permitted in public diagnostics.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

impl PublicError {
    /// Starts a checked public error using a static source-free message template.
    pub fn builder(code: ErrorCode, message: &'static str) -> PublicErrorBuilder {
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
    fn new(code: ErrorCode, message: &'static str) -> Self {
        let build_error =
            (message.len() > MAX_MESSAGE_BYTES).then_some(PublicErrorBuildError::MessageTooLong);
        Self {
            error: PublicError {
                code,
                message: message.to_owned(),
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
    /// The static message template exceeds the public bound.
    #[error("public error message exceeds its byte limit")]
    MessageTooLong,
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
    fn retry_delay_is_bounded() {
        let result = PublicError::builder(ErrorCode::Busy, "repository is busy")
            .retry_after(Duration::from_secs(24 * 60 * 60 + 1))
            .build();
        assert_eq!(result, Err(PublicErrorBuildError::RetryDelayTooLong));
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
}
