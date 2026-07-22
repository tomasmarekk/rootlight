//! Authoritative conversion from MCP domain failures to public error policy.
//!
//! The stable code, correctable field policy, and next action live here.
//! Message, retryability, wire identity, and remediation continue to come from
//! the shared `rootlight-error` registry through `error_definition`.

use rootlight_agent::{advanced::AdvancedQueryError, batch::BatchValidationError};
use rootlight_mcp_contract::{
    DetailKey, ErrorCode, NextAction, PublicError, PublicErrorBuildError, PublicValue,
    error_definition, pagination::CursorError,
};
use serde::Serialize;

#[cfg(test)]
pub(crate) const ERROR_MAPPING_VERSION: &str = "1.0";
const NO_DETAILS: &[&str] = &[];
const CAPABILITY_DETAILS: &[&str] = &["capability_reason", "field_path"];
const COST_DETAILS: &[&str] = &["cost_limit", "estimated_cost"];

/// Closed failure families currently converted at the MCP boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DomainFailureKind {
    InvalidArgument,
    TypeMismatch,
    InvalidCursor,
    UnsupportedCapability,
    IncompleteCoverage,
    BudgetExceeded,
    ResourceExhausted,
    CostLimit,
    OperatorForbidden,
    BindingInvalid,
    BindingTypeMismatch,
}

impl DomainFailureKind {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 11] = [
        Self::InvalidArgument,
        Self::TypeMismatch,
        Self::InvalidCursor,
        Self::UnsupportedCapability,
        Self::IncompleteCoverage,
        Self::BudgetExceeded,
        Self::ResourceExhausted,
        Self::CostLimit,
        Self::OperatorForbidden,
        Self::BindingInvalid,
        Self::BindingTypeMismatch,
    ];
}

/// Whether a mapping accepts or fixes the field named by a correction action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FieldPolicy {
    None,
    Required,
    Fixed,
}

/// Closed public action emitted for a mapped failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActionPolicy {
    CorrectField,
    RestartEnumeration,
    Retry,
}

/// Stable policy for one MCP domain failure family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct ErrorMapping {
    pub(crate) failure: DomainFailureKind,
    pub(crate) code: ErrorCode,
    pub(crate) field_policy: FieldPolicy,
    pub(crate) fixed_field: Option<&'static str>,
    pub(crate) next_action: ActionPolicy,
    pub(crate) allowed_detail_keys: &'static [&'static str],
}

pub(crate) const ERROR_MAPPINGS: [ErrorMapping; 11] = [
    ErrorMapping {
        failure: DomainFailureKind::InvalidArgument,
        code: ErrorCode::InvalidArgument,
        field_policy: FieldPolicy::Required,
        fixed_field: None,
        next_action: ActionPolicy::CorrectField,
        allowed_detail_keys: NO_DETAILS,
    },
    ErrorMapping {
        failure: DomainFailureKind::TypeMismatch,
        code: ErrorCode::TypeMismatch,
        field_policy: FieldPolicy::Required,
        fixed_field: None,
        next_action: ActionPolicy::CorrectField,
        allowed_detail_keys: NO_DETAILS,
    },
    ErrorMapping {
        failure: DomainFailureKind::InvalidCursor,
        code: ErrorCode::InvalidCursor,
        field_policy: FieldPolicy::None,
        fixed_field: None,
        next_action: ActionPolicy::RestartEnumeration,
        allowed_detail_keys: NO_DETAILS,
    },
    ErrorMapping {
        failure: DomainFailureKind::UnsupportedCapability,
        code: ErrorCode::UnsupportedCapability,
        field_policy: FieldPolicy::Required,
        fixed_field: None,
        next_action: ActionPolicy::CorrectField,
        allowed_detail_keys: CAPABILITY_DETAILS,
    },
    ErrorMapping {
        failure: DomainFailureKind::IncompleteCoverage,
        code: ErrorCode::IncompleteCoverage,
        field_policy: FieldPolicy::Fixed,
        fixed_field: Some("scope"),
        next_action: ActionPolicy::CorrectField,
        allowed_detail_keys: NO_DETAILS,
    },
    ErrorMapping {
        failure: DomainFailureKind::BudgetExceeded,
        code: ErrorCode::BudgetExceeded,
        field_policy: FieldPolicy::Fixed,
        fixed_field: Some("budget"),
        next_action: ActionPolicy::CorrectField,
        allowed_detail_keys: NO_DETAILS,
    },
    ErrorMapping {
        failure: DomainFailureKind::ResourceExhausted,
        code: ErrorCode::ResourceExhausted,
        field_policy: FieldPolicy::None,
        fixed_field: None,
        next_action: ActionPolicy::Retry,
        allowed_detail_keys: NO_DETAILS,
    },
    ErrorMapping {
        failure: DomainFailureKind::CostLimit,
        code: ErrorCode::CostLimit,
        field_policy: FieldPolicy::Required,
        fixed_field: None,
        next_action: ActionPolicy::CorrectField,
        allowed_detail_keys: COST_DETAILS,
    },
    ErrorMapping {
        failure: DomainFailureKind::OperatorForbidden,
        code: ErrorCode::OperatorForbidden,
        field_policy: FieldPolicy::Required,
        fixed_field: None,
        next_action: ActionPolicy::CorrectField,
        allowed_detail_keys: CAPABILITY_DETAILS,
    },
    ErrorMapping {
        failure: DomainFailureKind::BindingInvalid,
        code: ErrorCode::BindingInvalid,
        field_policy: FieldPolicy::Fixed,
        fixed_field: Some("arguments"),
        next_action: ActionPolicy::CorrectField,
        allowed_detail_keys: CAPABILITY_DETAILS,
    },
    ErrorMapping {
        failure: DomainFailureKind::BindingTypeMismatch,
        code: ErrorCode::BindingTypeMismatch,
        field_policy: FieldPolicy::Fixed,
        fixed_field: Some("arguments"),
        next_action: ActionPolicy::CorrectField,
        allowed_detail_keys: NO_DETAILS,
    },
];

/// One classified failure with any caller-owned field selected by its policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MappedDomainFailure {
    kind: DomainFailureKind,
    field: Option<&'static str>,
}

impl MappedDomainFailure {
    pub(crate) const fn invalid_argument(field: &'static str) -> Self {
        Self::with_field(DomainFailureKind::InvalidArgument, field)
    }

    pub(crate) const fn type_mismatch(field: &'static str) -> Self {
        Self::with_field(DomainFailureKind::TypeMismatch, field)
    }

    pub(crate) const fn invalid_cursor() -> Self {
        Self::without_field(DomainFailureKind::InvalidCursor)
    }

    pub(crate) const fn unsupported_capability(field: &'static str) -> Self {
        Self::with_field(DomainFailureKind::UnsupportedCapability, field)
    }

    #[cfg(test)]
    pub(crate) const fn incomplete_coverage() -> Self {
        Self::without_field(DomainFailureKind::IncompleteCoverage)
    }

    pub(crate) const fn budget_exceeded() -> Self {
        Self::without_field(DomainFailureKind::BudgetExceeded)
    }

    pub(crate) const fn resource_exhausted() -> Self {
        Self::without_field(DomainFailureKind::ResourceExhausted)
    }

    pub(crate) const fn cost_limit(field: &'static str) -> Self {
        Self::with_field(DomainFailureKind::CostLimit, field)
    }

    pub(crate) const fn operator_forbidden(field: &'static str) -> Self {
        Self::with_field(DomainFailureKind::OperatorForbidden, field)
    }

    pub(crate) const fn binding_invalid() -> Self {
        Self::without_field(DomainFailureKind::BindingInvalid)
    }

    pub(crate) const fn binding_type_mismatch() -> Self {
        Self::without_field(DomainFailureKind::BindingTypeMismatch)
    }

    const fn with_field(kind: DomainFailureKind, field: &'static str) -> Self {
        Self {
            kind,
            field: Some(field),
        }
    }

    const fn without_field(kind: DomainFailureKind) -> Self {
        Self { kind, field: None }
    }

    pub(crate) const fn kind(self) -> DomainFailureKind {
        self.kind
    }

    pub(crate) const fn field(self) -> Option<&'static str> {
        self.field
    }
}

impl From<AdvancedQueryError> for MappedDomainFailure {
    fn from(error: AdvancedQueryError) -> Self {
        match error {
            AdvancedQueryError::ForbiddenOperator => Self::operator_forbidden("query"),
            AdvancedQueryError::TypeMismatch => Self::type_mismatch("query"),
            AdvancedQueryError::MissingParameter
            | AdvancedQueryError::UnexpectedParameter
            | AdvancedQueryError::InvalidParameter
            | AdvancedQueryError::ParameterSizeExceeded => Self::invalid_argument("parameters"),
            AdvancedQueryError::CostExceeded
            | AdvancedQueryError::DepthExceeded
            | AdvancedQueryError::RowLimitExceeded
            | AdvancedQueryError::TraversalLimitExceeded => Self::cost_limit("query"),
            AdvancedQueryError::Malformed => Self::invalid_argument("query"),
        }
    }
}

impl From<BatchValidationError> for MappedDomainFailure {
    fn from(error: BatchValidationError) -> Self {
        match error {
            BatchValidationError::ForbiddenTool | BatchValidationError::NestedBatch => {
                Self::operator_forbidden("operations")
            }
            BatchValidationError::InvalidBinding => Self::binding_invalid(),
            BatchValidationError::InvalidOperationCount
            | BatchValidationError::CyclicDependency
            | BatchValidationError::DepthExceeded
            | BatchValidationError::InvalidDependencyReference
            | BatchValidationError::TooManyDependencies => Self::invalid_argument("operations"),
        }
    }
}

impl From<CursorError> for MappedDomainFailure {
    fn from(error: CursorError) -> Self {
        match error {
            CursorError::TooLong
            | CursorError::Malformed
            | CursorError::RepositoryMismatch
            | CursorError::GenerationMismatch
            | CursorError::QueryMismatch
            | CursorError::Expired
            | CursorError::IntegrityFailed
            | CursorError::IssuedInTheFuture
            | CursorError::KeyMismatch
            | CursorError::UnsupportedVersion
            | CursorError::TimestampOverflow => Self::invalid_cursor(),
        }
    }
}

pub(crate) fn mapping_for(kind: DomainFailureKind) -> &'static ErrorMapping {
    match kind {
        DomainFailureKind::InvalidArgument => &ERROR_MAPPINGS[0],
        DomainFailureKind::TypeMismatch => &ERROR_MAPPINGS[1],
        DomainFailureKind::InvalidCursor => &ERROR_MAPPINGS[2],
        DomainFailureKind::UnsupportedCapability => &ERROR_MAPPINGS[3],
        DomainFailureKind::IncompleteCoverage => &ERROR_MAPPINGS[4],
        DomainFailureKind::BudgetExceeded => &ERROR_MAPPINGS[5],
        DomainFailureKind::ResourceExhausted => &ERROR_MAPPINGS[6],
        DomainFailureKind::CostLimit => &ERROR_MAPPINGS[7],
        DomainFailureKind::OperatorForbidden => &ERROR_MAPPINGS[8],
        DomainFailureKind::BindingInvalid => &ERROR_MAPPINGS[9],
        DomainFailureKind::BindingTypeMismatch => &ERROR_MAPPINGS[10],
    }
}

/// Builds the checked public envelope defined by the authoritative mapping.
///
/// # Errors
///
/// Returns a checked public-error construction failure if a supplied field
/// violates the bounded detail-key grammar.
pub(crate) fn public_error(
    failure: MappedDomainFailure,
) -> Result<PublicError, PublicErrorBuildError> {
    public_error_with_details(failure, [])
}

/// Builds the checked public envelope with mapping-approved typed details.
///
/// # Errors
///
/// Returns a checked public-error construction failure if a supplied detail is
/// not declared for the failure family or violates the public envelope bounds.
pub(crate) fn public_error_with_details(
    failure: MappedDomainFailure,
    details: impl IntoIterator<Item = (DetailKey, PublicValue)>,
) -> Result<PublicError, PublicErrorBuildError> {
    let mapping = mapping_for(failure.kind());
    let definition = error_definition(mapping.code);
    let mut builder = PublicError::builder(mapping.code, definition.message);
    if definition.retryable {
        builder = builder.retryable();
    }
    let field = match mapping.field_policy {
        FieldPolicy::None => None,
        FieldPolicy::Required => failure.field(),
        FieldPolicy::Fixed => mapping.fixed_field,
    };
    builder = match mapping.next_action {
        ActionPolicy::CorrectField => builder.next_action(NextAction::CorrectField {
            field: DetailKey::parse(field.ok_or(PublicErrorBuildError::InvalidDetailKey)?)?,
        }),
        ActionPolicy::RestartEnumeration => {
            debug_assert!(field.is_none());
            builder.next_action(NextAction::RestartEnumeration)
        }
        ActionPolicy::Retry => {
            debug_assert!(field.is_none());
            builder.next_action(NextAction::Retry)
        }
    };
    for (key, value) in details {
        if !mapping.allowed_detail_keys.contains(&key.as_str()) {
            return Err(PublicErrorBuildError::InvalidDetailKey);
        }
        builder = builder.detail(key, value);
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use rootlight_agent::{advanced::AdvancedQueryError, batch::BatchValidationError};
    use rootlight_mcp_contract::{
        ErrorCode, NextAction, error_definition, pagination::CursorError,
    };
    use serde::Serialize;

    use super::{
        ActionPolicy, DomainFailureKind, ERROR_MAPPING_VERSION, ERROR_MAPPINGS, ErrorMapping,
        FieldPolicy, MappedDomainFailure, mapping_for, public_error, public_error_with_details,
    };

    #[derive(Serialize)]
    struct MappingArtifact {
        schema_version: &'static str,
        mappings: Vec<MappingArtifactEntry>,
    }

    #[derive(Serialize)]
    struct MappingArtifactEntry {
        failure: DomainFailureKind,
        code: ErrorCode,
        wire_number: i32,
        message: &'static str,
        retryable: bool,
        remediation: String,
        field_policy: FieldPolicy,
        fixed_field: Option<&'static str>,
        next_action: ActionPolicy,
        allowed_detail_keys: &'static [&'static str],
    }

    fn artifact() -> MappingArtifact {
        MappingArtifact {
            schema_version: ERROR_MAPPING_VERSION,
            mappings: ERROR_MAPPINGS
                .iter()
                .map(|mapping| {
                    let definition = error_definition(mapping.code);
                    MappingArtifactEntry {
                        failure: mapping.failure,
                        code: mapping.code,
                        wire_number: definition.wire_number,
                        message: definition.message,
                        retryable: definition.retryable,
                        remediation: format!("{:?}", definition.remediation),
                        field_policy: mapping.field_policy,
                        fixed_field: mapping.fixed_field,
                        next_action: mapping.next_action,
                        allowed_detail_keys: mapping.allowed_detail_keys,
                    }
                })
                .collect(),
        }
    }

    fn representative_failure(mapping: ErrorMapping) -> MappedDomainFailure {
        match mapping.failure {
            DomainFailureKind::InvalidArgument => {
                MappedDomainFailure::invalid_argument("arguments")
            }
            DomainFailureKind::TypeMismatch => MappedDomainFailure::type_mismatch("arguments"),
            DomainFailureKind::InvalidCursor => MappedDomainFailure::invalid_cursor(),
            DomainFailureKind::UnsupportedCapability => {
                MappedDomainFailure::unsupported_capability("operations")
            }
            DomainFailureKind::IncompleteCoverage => MappedDomainFailure::incomplete_coverage(),
            DomainFailureKind::BudgetExceeded => MappedDomainFailure::budget_exceeded(),
            DomainFailureKind::ResourceExhausted => MappedDomainFailure::resource_exhausted(),
            DomainFailureKind::CostLimit => MappedDomainFailure::cost_limit("cost_limit"),
            DomainFailureKind::OperatorForbidden => {
                MappedDomainFailure::operator_forbidden("query")
            }
            DomainFailureKind::BindingInvalid => MappedDomainFailure::binding_invalid(),
            DomainFailureKind::BindingTypeMismatch => MappedDomainFailure::binding_type_mismatch(),
        }
    }

    #[test]
    fn mapping_table_is_exhaustive_and_unique() {
        assert_eq!(ERROR_MAPPINGS.len(), DomainFailureKind::ALL.len());
        let failures: BTreeSet<_> = ERROR_MAPPINGS
            .iter()
            .map(|mapping| mapping.failure)
            .collect();
        let codes: BTreeSet<_> = ERROR_MAPPINGS
            .iter()
            .map(|mapping| mapping.code.wire_number())
            .collect();
        assert_eq!(failures.len(), ERROR_MAPPINGS.len());
        assert_eq!(codes.len(), ERROR_MAPPINGS.len());

        for failure in DomainFailureKind::ALL {
            assert_eq!(mapping_for(failure).failure, failure);
        }
    }

    #[test]
    fn every_mapping_uses_the_shared_registry_definition() {
        for mapping in ERROR_MAPPINGS {
            let definition = error_definition(mapping.code);
            let error =
                public_error(representative_failure(mapping)).expect("mapping builds an envelope");
            assert_eq!(error.code(), definition.code);
            assert_eq!(error.message(), definition.message);
            assert_eq!(error.retryable(), definition.retryable);
            assert_eq!(error.next_actions().len(), 1);
            let expected_action = match format!("{:?}", definition.remediation).as_str() {
                "CorrectInput" | "ReduceScope" => ActionPolicy::CorrectField,
                "RestartEnumeration" => ActionPolicy::RestartEnumeration,
                "Retry" => ActionPolicy::Retry,
                other => panic!("mapped failure uses unsupported remediation {other}"),
            };
            assert_eq!(mapping.next_action, expected_action);
        }
    }

    #[test]
    fn authoritative_envelopes_are_bounded_and_source_free() {
        for mapping in ERROR_MAPPINGS {
            let error =
                public_error(representative_failure(mapping)).expect("mapping builds an envelope");
            let encoded = serde_json::to_string(&error).expect("checked envelope serializes");
            assert!(error.message().len() <= 1_024);
            assert!(error.details().len() <= 32);
            assert!(error.next_actions().len() <= 8);
            for forbidden in [
                "C:\\",
                "/home/",
                "BEGIN PRIVATE KEY",
                "gho_",
                "operations.0.arguments.secret",
            ] {
                assert!(!encoded.contains(forbidden));
            }
        }
    }

    #[test]
    fn detail_enrichment_is_closed_by_failure_family() {
        let field_path =
            rootlight_mcp_contract::DetailKey::parse("field_path").expect("static key is valid");
        let label = rootlight_mcp_contract::SafeLabel::parse("query")
            .expect("static detail label is valid");
        let enriched = public_error_with_details(
            MappedDomainFailure::unsupported_capability("arguments"),
            [(
                field_path.clone(),
                rootlight_mcp_contract::PublicValue::Label(label),
            )],
        )
        .expect("declared capability detail is accepted");
        assert!(enriched.details().contains_key(&field_path));

        assert_eq!(
            public_error_with_details(
                MappedDomainFailure::budget_exceeded(),
                [(
                    field_path,
                    rootlight_mcp_contract::PublicValue::Boolean(true),
                )],
            ),
            Err(rootlight_mcp_contract::PublicErrorBuildError::InvalidDetailKey)
        );
    }

    #[test]
    fn advanced_query_conversion_is_exhaustive_and_stable() {
        let cases = [
            (
                AdvancedQueryError::DepthExceeded,
                DomainFailureKind::CostLimit,
                Some("query"),
            ),
            (
                AdvancedQueryError::ForbiddenOperator,
                DomainFailureKind::OperatorForbidden,
                Some("query"),
            ),
            (
                AdvancedQueryError::RowLimitExceeded,
                DomainFailureKind::CostLimit,
                Some("query"),
            ),
            (
                AdvancedQueryError::TraversalLimitExceeded,
                DomainFailureKind::CostLimit,
                Some("query"),
            ),
            (
                AdvancedQueryError::CostExceeded,
                DomainFailureKind::CostLimit,
                Some("query"),
            ),
            (
                AdvancedQueryError::Malformed,
                DomainFailureKind::InvalidArgument,
                Some("query"),
            ),
            (
                AdvancedQueryError::TypeMismatch,
                DomainFailureKind::TypeMismatch,
                Some("query"),
            ),
        ];
        for (input, kind, field) in cases {
            let mapped = MappedDomainFailure::from(input);
            assert_eq!(mapped.kind(), kind);
            assert_eq!(mapped.field(), field);
        }
    }

    #[test]
    fn batch_validation_conversion_is_exhaustive_and_stable() {
        let cases = [
            (
                BatchValidationError::InvalidOperationCount,
                DomainFailureKind::InvalidArgument,
                Some("operations"),
            ),
            (
                BatchValidationError::ForbiddenTool,
                DomainFailureKind::OperatorForbidden,
                Some("operations"),
            ),
            (
                BatchValidationError::CyclicDependency,
                DomainFailureKind::InvalidArgument,
                Some("operations"),
            ),
            (
                BatchValidationError::DepthExceeded,
                DomainFailureKind::InvalidArgument,
                Some("operations"),
            ),
            (
                BatchValidationError::InvalidDependencyReference,
                DomainFailureKind::InvalidArgument,
                Some("operations"),
            ),
            (
                BatchValidationError::TooManyDependencies,
                DomainFailureKind::InvalidArgument,
                Some("operations"),
            ),
            (
                BatchValidationError::InvalidBinding,
                DomainFailureKind::BindingInvalid,
                None,
            ),
            (
                BatchValidationError::NestedBatch,
                DomainFailureKind::OperatorForbidden,
                Some("operations"),
            ),
        ];
        for (input, kind, field) in cases {
            let mapped = MappedDomainFailure::from(input);
            assert_eq!(mapped.kind(), kind);
            assert_eq!(mapped.field(), field);
        }
    }

    #[test]
    fn every_cursor_failure_restarts_enumeration() {
        for cursor_error in [
            CursorError::TooLong,
            CursorError::Malformed,
            CursorError::RepositoryMismatch,
            CursorError::GenerationMismatch,
            CursorError::QueryMismatch,
            CursorError::Expired,
            CursorError::IntegrityFailed,
            CursorError::IssuedInTheFuture,
            CursorError::KeyMismatch,
            CursorError::UnsupportedVersion,
            CursorError::TimestampOverflow,
        ] {
            let error = public_error(cursor_error.into()).expect("cursor mapping builds");
            assert_eq!(error.code(), ErrorCode::InvalidCursor);
            assert_eq!(error.next_actions(), &[NextAction::RestartEnumeration]);
        }
    }

    #[test]
    fn versioned_mapping_artifact_is_stable() {
        let observed = format!(
            "{}\n",
            serde_json::to_string_pretty(&artifact()).expect("mapping artifact serializes")
        );
        assert_eq!(
            observed,
            include_str!("../../../tests/fixtures/errors/error-mapping-1.0.json")
        );
    }
}
