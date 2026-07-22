//! Provider-neutral token-accounting evidence.
//!
//! Runtime limits remain independent of this module. These types describe
//! offline evidence where byte counts, deterministic estimates, and counts
//! from an identified tokenizer must remain distinguishable.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema version for workflow token-accounting evidence.
pub const TOKEN_ACCOUNTING_SCHEMA_VERSION: &str = "1.0";

const SHA256_HEX_LENGTH: usize = 64;

/// Identity and provenance of the tokenizer used for actual counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActualTokenizerIdentity {
    /// Provider whose message accounting is being approximated.
    pub provider: String,
    /// Model whose tokenizer contract is being measured.
    pub model: String,
    /// Stable tokenizer or vocabulary name.
    pub tokenizer: String,
    /// Library or executable that performed the count.
    pub implementation: String,
    /// Pinned implementation version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implementation_version: Option<String>,
    /// SHA-256 digest of the implementation package or executable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implementation_sha256: Option<String>,
    /// SHA-256 digest of separately distributed tokenizer assets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_sha256: Option<String>,
}

impl ActualTokenizerIdentity {
    fn validate(&self) -> Result<(), TokenAccountingError> {
        require_non_empty("tokenizer.provider", &self.provider)?;
        require_non_empty("tokenizer.model", &self.model)?;
        require_non_empty("tokenizer.tokenizer", &self.tokenizer)?;
        require_non_empty("tokenizer.implementation", &self.implementation)?;

        if self
            .implementation_version
            .as_deref()
            .is_some_and(str::is_empty)
        {
            return Err(TokenAccountingError::EmptyField(
                "tokenizer.implementation_version",
            ));
        }
        validate_optional_sha256(
            "tokenizer.implementation_sha256",
            self.implementation_sha256.as_deref(),
        )?;
        validate_optional_sha256("tokenizer.asset_sha256", self.asset_sha256.as_deref())?;

        if self.implementation_version.is_none()
            && self.implementation_sha256.is_none()
            && self.asset_sha256.is_none()
        {
            return Err(TokenAccountingError::UnpinnedTokenizer);
        }
        Ok(())
    }
}

/// Stable semantic boundary of one measured tokenizer input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenInputKind {
    /// Complete serialized request frame.
    Request,
    /// Complete serialized response frame.
    Response,
    /// Source text attributed within a response.
    Source,
    /// Complete serialized `tools/list` payload.
    ToolList,
    /// One batch child-operation frame.
    BatchOperation,
    /// One named context-pack section.
    ContextSection,
}

/// One exact-input byte, estimate, and optional actual-token measurement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenMeasurement {
    /// Semantic boundary of the measured input.
    pub input_kind: TokenInputKind,
    /// SHA-256 of the exact bytes passed to the tokenizer.
    pub input_sha256: String,
    /// Exact UTF-8 byte length of the tokenizer input.
    pub serialized_bytes: u64,
    /// Provider-neutral estimate retained independently of tokenizer support.
    pub deterministic_estimated_tokens: u64,
    /// Count returned by the report's identified tokenizer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_tokens: Option<u64>,
    /// Transformation applied before tokenization, such as `none`.
    pub normalization: String,
    /// Message or section framing included in the exact input.
    pub framing: String,
}

impl TokenMeasurement {
    /// Builds a measurement from the exact bytes presented to a tokenizer.
    #[must_use]
    pub fn from_input(
        input_kind: TokenInputKind,
        input: &[u8],
        deterministic_estimated_tokens: u64,
        actual_tokens: Option<u64>,
        normalization: impl Into<String>,
        framing: impl Into<String>,
    ) -> Self {
        Self {
            input_kind,
            input_sha256: sha256_hex(input),
            serialized_bytes: u64::try_from(input.len()).unwrap_or(u64::MAX),
            deterministic_estimated_tokens,
            actual_tokens,
            normalization: normalization.into(),
            framing: framing.into(),
        }
    }

    /// Checks structural invariants that do not require the original input.
    ///
    /// # Errors
    ///
    /// Returns [`TokenAccountingError`] when a digest or boundary label is
    /// non-canonical.
    pub fn validate(&self) -> Result<(), TokenAccountingError> {
        validate_sha256("input_sha256", &self.input_sha256)?;
        require_non_empty("normalization", &self.normalization)?;
        require_non_empty("framing", &self.framing)?;
        Ok(())
    }

    /// Verifies that the retained digest and byte count bind this measurement
    /// to the exact tokenizer input.
    ///
    /// # Errors
    ///
    /// Returns [`TokenAccountingError`] when the measurement is malformed or
    /// the supplied input differs in length or digest.
    pub fn verify_input(&self, input: &[u8]) -> Result<(), TokenAccountingError> {
        self.validate()?;
        let observed_bytes =
            u64::try_from(input.len()).map_err(|_| TokenAccountingError::IntegerOverflow)?;
        let observed_sha256 = sha256_hex(input);
        if self.serialized_bytes != observed_bytes || self.input_sha256 != observed_sha256 {
            return Err(TokenAccountingError::InputMismatch {
                expected_bytes: self.serialized_bytes,
                observed_bytes,
                expected_sha256: self.input_sha256.clone(),
                observed_sha256,
            });
        }
        Ok(())
    }
}

/// Derived workflow totals.
///
/// Source accounting is attribution within the response and is intentionally
/// excluded from this sum. Adding it again would double-count returned source
/// content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenTotals {
    /// Request bytes plus response bytes.
    pub serialized_bytes: u64,
    /// Request estimate plus response estimate.
    pub deterministic_estimated_tokens: u64,
    /// Request actual count plus response actual count, when both exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_tokens: Option<u64>,
}

/// Complete token evidence for one measured workflow invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowTokenAccounting {
    /// Evidence schema version.
    pub schema: String,
    /// Tokenizer identity when any measurement contains an actual count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokenizer: Option<ActualTokenizerIdentity>,
    /// Complete request frame.
    pub request: TokenMeasurement,
    /// Complete response frame, including returned source content.
    pub response: TokenMeasurement,
    /// Source-only attribution using its stated framing.
    pub source: TokenMeasurement,
    /// Derived request-plus-response totals.
    pub total: TokenTotals,
}

impl WorkflowTokenAccounting {
    /// Constructs internally consistent workflow evidence.
    ///
    /// # Errors
    ///
    /// Returns [`TokenAccountingError`] when identity, input boundaries, or
    /// derived totals are incomplete or inconsistent.
    pub fn new(
        tokenizer: Option<ActualTokenizerIdentity>,
        request: TokenMeasurement,
        response: TokenMeasurement,
        source: TokenMeasurement,
    ) -> Result<Self, TokenAccountingError> {
        let total = totals(&request, &response)?;
        let evidence = Self {
            schema: TOKEN_ACCOUNTING_SCHEMA_VERSION.to_owned(),
            tokenizer,
            request,
            response,
            source,
            total,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    /// Validates identity, boundaries, and all derived totals.
    ///
    /// # Errors
    ///
    /// Returns [`TokenAccountingError`] when the schema, tokenizer identity,
    /// component measurements, or derived totals violate the evidence
    /// contract.
    pub fn validate(&self) -> Result<(), TokenAccountingError> {
        if self.schema != TOKEN_ACCOUNTING_SCHEMA_VERSION {
            return Err(TokenAccountingError::UnsupportedSchema(self.schema.clone()));
        }
        self.request.validate()?;
        self.response.validate()?;
        self.source.validate()?;
        expect_kind("request", self.request.input_kind, TokenInputKind::Request)?;
        expect_kind(
            "response",
            self.response.input_kind,
            TokenInputKind::Response,
        )?;
        expect_kind("source", self.source.input_kind, TokenInputKind::Source)?;

        let has_actual = self.request.actual_tokens.is_some()
            || self.response.actual_tokens.is_some()
            || self.source.actual_tokens.is_some();
        if has_actual {
            self.tokenizer
                .as_ref()
                .ok_or(TokenAccountingError::MissingTokenizerIdentity)?
                .validate()?;
        } else if let Some(tokenizer) = &self.tokenizer {
            tokenizer.validate()?;
        }

        if self.source.serialized_bytes > self.response.serialized_bytes {
            return Err(TokenAccountingError::SourceExceedsResponse);
        }

        let expected = totals(&self.request, &self.response)?;
        if self.total != expected {
            return Err(TokenAccountingError::TotalsMismatch {
                expected,
                observed: self.total,
            });
        }
        Ok(())
    }
}

fn totals(
    request: &TokenMeasurement,
    response: &TokenMeasurement,
) -> Result<TokenTotals, TokenAccountingError> {
    let serialized_bytes = request
        .serialized_bytes
        .checked_add(response.serialized_bytes)
        .ok_or(TokenAccountingError::IntegerOverflow)?;
    let deterministic_estimated_tokens = request
        .deterministic_estimated_tokens
        .checked_add(response.deterministic_estimated_tokens)
        .ok_or(TokenAccountingError::IntegerOverflow)?;
    let actual_tokens = match (request.actual_tokens, response.actual_tokens) {
        (Some(request), Some(response)) => Some(
            request
                .checked_add(response)
                .ok_or(TokenAccountingError::IntegerOverflow)?,
        ),
        (None, None) => None,
        _ => return Err(TokenAccountingError::PartialActualTotals),
    };
    Ok(TokenTotals {
        serialized_bytes,
        deterministic_estimated_tokens,
        actual_tokens,
    })
}

fn expect_kind(
    field: &'static str,
    observed: TokenInputKind,
    expected: TokenInputKind,
) -> Result<(), TokenAccountingError> {
    if observed != expected {
        return Err(TokenAccountingError::InputKind {
            field,
            expected,
            observed,
        });
    }
    Ok(())
}

fn require_non_empty(field: &'static str, value: &str) -> Result<(), TokenAccountingError> {
    if value.is_empty() {
        return Err(TokenAccountingError::EmptyField(field));
    }
    Ok(())
}

fn validate_optional_sha256(
    field: &'static str,
    value: Option<&str>,
) -> Result<(), TokenAccountingError> {
    if let Some(value) = value {
        validate_sha256(field, value)?;
    }
    Ok(())
}

fn validate_sha256(field: &'static str, value: &str) -> Result<(), TokenAccountingError> {
    if value.len() != SHA256_HEX_LENGTH
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(TokenAccountingError::InvalidSha256(field));
    }
    Ok(())
}

/// Returns a lowercase SHA-256 digest for an exact tokenizer input.
#[must_use]
pub fn sha256_hex(input: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(input);
    let mut encoded = String::with_capacity(SHA256_HEX_LENGTH);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

/// Invalid or internally inconsistent token-accounting evidence.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenAccountingError {
    /// Evidence uses an unknown schema.
    #[error("unsupported token-accounting schema: {0}")]
    UnsupportedSchema(String),
    /// A required identity or input-boundary field is empty.
    #[error("token-accounting field is empty: {0}")]
    EmptyField(&'static str),
    /// A digest is not lowercase SHA-256 hexadecimal.
    #[error("token-accounting field is not a lowercase SHA-256 digest: {0}")]
    InvalidSha256(&'static str),
    /// Actual counts do not identify their tokenizer.
    #[error("actual token counts require tokenizer identity")]
    MissingTokenizerIdentity,
    /// Tokenizer provenance has neither a version nor an implementation or
    /// asset digest.
    #[error("tokenizer identity is not pinned by version or digest")]
    UnpinnedTokenizer,
    /// A workflow field carries the wrong semantic input boundary.
    #[error("{field} input kind differs: expected {expected:?}, observed {observed:?}")]
    InputKind {
        /// Workflow field being validated.
        field: &'static str,
        /// Required semantic boundary.
        expected: TokenInputKind,
        /// Reported semantic boundary.
        observed: TokenInputKind,
    },
    /// Only one side of a request-plus-response actual total was measured.
    #[error("workflow actual totals require both request and response counts")]
    PartialActualTotals,
    /// Source attribution cannot exceed its containing response frame.
    #[error("source byte attribution exceeds the complete response frame")]
    SourceExceedsResponse,
    /// Stored totals differ from their request and response components.
    #[error("workflow token totals differ: expected {expected:?}, observed {observed:?}")]
    TotalsMismatch {
        /// Recomputed totals.
        expected: TokenTotals,
        /// Stored totals.
        observed: TokenTotals,
    },
    /// Original bytes differ from the measurement binding.
    #[error(
        "tokenizer input differs: expected {expected_bytes} bytes/{expected_sha256}, observed {observed_bytes} bytes/{observed_sha256}"
    )]
    InputMismatch {
        /// Stored byte count.
        expected_bytes: u64,
        /// Observed byte count.
        observed_bytes: u64,
        /// Stored digest.
        expected_sha256: String,
        /// Observed digest.
        observed_sha256: String,
    },
    /// An accounting sum or platform conversion overflowed.
    #[error("token-accounting integer overflow")]
    IntegerOverflow,
}

#[cfg(test)]
mod tests {
    use super::{
        ActualTokenizerIdentity, TokenAccountingError, TokenInputKind, TokenMeasurement,
        WorkflowTokenAccounting,
    };

    fn tokenizer() -> ActualTokenizerIdentity {
        ActualTokenizerIdentity {
            provider: "example".to_owned(),
            model: "example-model".to_owned(),
            tokenizer: "example-vocabulary".to_owned(),
            implementation: "example-tokenizer".to_owned(),
            implementation_version: Some("1.2.3".to_owned()),
            implementation_sha256: None,
            asset_sha256: Some("a".repeat(64)),
        }
    }

    fn measurement(
        kind: TokenInputKind,
        input: &[u8],
        estimate: u64,
        actual: Option<u64>,
    ) -> TokenMeasurement {
        TokenMeasurement::from_input(kind, input, estimate, actual, "none", "exact_bytes")
    }

    #[test]
    fn workflow_separates_source_attribution_from_total() {
        let request = measurement(TokenInputKind::Request, br#"{"request":1}"#, 4, Some(5));
        let response = measurement(
            TokenInputKind::Response,
            br#"{"source":"fn main() {}"}"#,
            8,
            Some(9),
        );
        let source = measurement(TokenInputKind::Source, b"fn main() {}", 3, Some(4));

        let evidence = WorkflowTokenAccounting::new(
            Some(tokenizer()),
            request.clone(),
            response.clone(),
            source.clone(),
        )
        .expect("workflow evidence validates");

        assert_eq!(
            evidence.total.serialized_bytes,
            request.serialized_bytes + response.serialized_bytes
        );
        assert_eq!(evidence.total.deterministic_estimated_tokens, 12);
        assert_eq!(evidence.total.actual_tokens, Some(14));
        assert_ne!(
            evidence.total.serialized_bytes,
            request.serialized_bytes + response.serialized_bytes + source.serialized_bytes
        );
    }

    #[test]
    fn actual_counts_require_pinned_tokenizer_identity() {
        let request = measurement(TokenInputKind::Request, b"request", 2, Some(2));
        let response = measurement(TokenInputKind::Response, b"response", 2, Some(2));
        let source = measurement(TokenInputKind::Source, b"", 0, Some(0));

        assert_eq!(
            WorkflowTokenAccounting::new(None, request, response, source),
            Err(TokenAccountingError::MissingTokenizerIdentity)
        );
    }

    #[test]
    fn exact_input_digest_is_verified() {
        let measurement = measurement(TokenInputKind::Request, b"exact request", 3, None);

        measurement
            .verify_input(b"exact request")
            .expect("original input verifies");
        assert!(matches!(
            measurement.verify_input(b"changed request"),
            Err(TokenAccountingError::InputMismatch { .. })
        ));
    }

    #[test]
    fn deserialized_tampering_fails_validation() {
        let request = measurement(TokenInputKind::Request, b"request", 2, None);
        let response = measurement(TokenInputKind::Response, b"response", 2, None);
        let source = measurement(TokenInputKind::Source, b"", 0, None);
        let evidence = WorkflowTokenAccounting::new(None, request, response, source)
            .expect("estimate-only evidence validates");
        let mut value = serde_json::to_value(evidence).expect("evidence serializes");
        value["total"]["serialized_bytes"] = serde_json::json!(999);
        let tampered: WorkflowTokenAccounting =
            serde_json::from_value(value).expect("shape remains valid");

        assert!(matches!(
            tampered.validate(),
            Err(TokenAccountingError::TotalsMismatch { .. })
        ));
    }

    #[test]
    fn partial_actual_totals_are_rejected() {
        let request = measurement(TokenInputKind::Request, b"request", 2, Some(2));
        let response = measurement(TokenInputKind::Response, b"response", 2, None);
        let source = measurement(TokenInputKind::Source, b"", 0, None);

        assert_eq!(
            WorkflowTokenAccounting::new(Some(tokenizer()), request, response, source),
            Err(TokenAccountingError::PartialActualTotals)
        );
    }
}
