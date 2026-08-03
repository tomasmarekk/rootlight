//! Source-free quick checks and single-use local support bundle downloads.

use std::time::{Duration, Instant};

use axum::{
    Extension, Json,
    body::Body,
    extract::{Path, State},
    http::{
        HeaderValue, StatusCode,
        header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::Response,
};
use data_encoding::HEXLOWER;
use rootlight_client::{DiagnosticOutcome, DiagnosticsQuick, HealthStatus, RequestTimeout};
use serde::Serialize;

use crate::{
    app::{ApiError, AppState},
    session::AuthenticatedSession,
    support_registry::{SupportArtifact, SupportRegistryError},
};

const QUICK_TIMEOUT: Duration = Duration::from_secs(8);
const SUPPORT_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuickDiagnosticsResponse {
    schema: &'static str,
    schema_version: u32,
    overall_status: &'static str,
    duration_ms: u32,
    checks: Vec<DiagnosticCheckResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticCheckResponse {
    name: &'static str,
    outcome: &'static str,
    duration_ms: u32,
    error: Option<DiagnosticErrorResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticErrorResponse {
    code: i32,
    message: String,
    retryable: bool,
    retry_after_ms: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SupportBundleResponse {
    schema: &'static str,
    receipt: String,
    download_path: String,
    archive_bytes: String,
    sha256: String,
    contains_source: bool,
    expires_in_seconds: u64,
}

pub(crate) async fn quick(
    State(state): State<AppState>,
) -> Result<Json<QuickDiagnosticsResponse>, ApiError> {
    let diagnostics = state
        .daemon()
        .diagnostics_quick(
            RequestTimeout::try_from(QUICK_TIMEOUT)
                .map_err(|_| ApiError::diagnostics_unavailable())?,
        )
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;
    Ok(Json(map_quick_diagnostics(diagnostics)))
}

pub(crate) async fn create_support_bundle(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<SupportBundleResponse>, ApiError> {
    let now = Instant::now();
    state
        .support()
        .reserve(session.identity(), now)
        .map_err(support_registry_error)?;
    let bundle = match state
        .daemon()
        .support_bundle(
            RequestTimeout::try_from(SUPPORT_TIMEOUT)
                .map_err(|_| ApiError::diagnostics_unavailable())?,
        )
        .await
    {
        Ok(bundle) => bundle,
        Err(error) => {
            state.support().abort(session.identity());
            return Err(ApiError::from_daemon(&error));
        }
    };
    if bundle.contains_source {
        state.support().abort(session.identity());
        return Err(ApiError::support_bundle_invalid());
    }
    let issued = state
        .support()
        .issue_reserved(
            session.identity(),
            bundle.archive,
            bundle.sha256,
            Instant::now(),
        )
        .map_err(support_registry_error)?;
    Ok(Json(SupportBundleResponse {
        schema: "rootlight.web-support-bundle/1",
        download_path: format!("/api/v1/diagnostics/support-bundles/{}", issued.receipt),
        receipt: issued.receipt,
        archive_bytes: issued.archive_bytes.to_string(),
        sha256: HEXLOWER.encode(&issued.sha256),
        contains_source: false,
        expires_in_seconds: issued.expires_in_seconds,
    }))
}

pub(crate) async fn download_support_bundle(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(receipt): Path<String>,
) -> Result<Response, ApiError> {
    let artifact = state
        .support()
        .take(session.identity(), &receipt, Instant::now())
        .map_err(support_registry_error)?;
    support_download(artifact)
}

fn map_quick_diagnostics(diagnostics: DiagnosticsQuick) -> QuickDiagnosticsResponse {
    let catalog = diagnostics.catalog;
    let duration_ms = catalog.duration_ms;
    let error = catalog.error.as_ref().map(|error| DiagnosticErrorResponse {
        code: error.code().wire_number(),
        message: error.message().to_owned(),
        retryable: error.retryable(),
        retry_after_ms: error.retry_after_ms().map(|delay| delay.to_string()),
    });
    QuickDiagnosticsResponse {
        schema: "rootlight.web-quick-diagnostics/1",
        schema_version: diagnostics.schema_version,
        overall_status: health_status(diagnostics.overall_status),
        duration_ms,
        checks: vec![DiagnosticCheckResponse {
            name: "catalog",
            outcome: diagnostic_outcome(catalog.outcome),
            duration_ms: catalog.duration_ms,
            error,
        }],
    }
}

fn support_download(artifact: SupportArtifact) -> Result<Response, ApiError> {
    let content_length = HeaderValue::from_str(&artifact.archive.len().to_string())
        .map_err(|_| ApiError::support_bundle_invalid())?;
    let digest = HeaderValue::from_str(&HEXLOWER.encode(&artifact.sha256))
        .map_err(|_| ApiError::support_bundle_invalid())?;
    let mut response = Response::new(Body::from(artifact.archive));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"rootlight-support-bundle.zip\""),
    );
    response
        .headers_mut()
        .insert(CONTENT_LENGTH, content_length);
    response.headers_mut().insert("x-rootlight-sha256", digest);
    Ok(response)
}

const fn health_status(value: HealthStatus) -> &'static str {
    match value {
        HealthStatus::Healthy => "healthy",
        HealthStatus::Degraded => "degraded",
        HealthStatus::Unavailable => "unavailable",
        HealthStatus::NotConfigured => "not_configured",
        HealthStatus::Failed => "failed",
    }
}

const fn diagnostic_outcome(value: DiagnosticOutcome) -> &'static str {
    match value {
        DiagnosticOutcome::Passed => "passed",
        DiagnosticOutcome::Failed => "failed",
        DiagnosticOutcome::TimedOut => "timed_out",
        DiagnosticOutcome::Unavailable => "unavailable",
    }
}

fn support_registry_error(error: SupportRegistryError) -> ApiError {
    match error {
        SupportRegistryError::Invalid => ApiError::support_bundle_not_found(),
        SupportRegistryError::LimitReached => ApiError::support_bundle_busy(),
        SupportRegistryError::ArchiveInvalid => ApiError::support_bundle_invalid(),
        SupportRegistryError::ResourceUnavailable => ApiError::diagnostics_unavailable(),
    }
}

#[cfg(test)]
mod tests {
    use rootlight_client::{DiagnosticResult, DiagnosticsQuick, HealthStatus};
    use sha2::Digest as _;

    use super::*;

    #[test]
    fn quick_diagnostics_preserve_real_outcome_without_fake_checks() {
        let mapped = map_quick_diagnostics(DiagnosticsQuick {
            schema_version: 1,
            overall_status: HealthStatus::Degraded,
            catalog: DiagnosticResult {
                outcome: DiagnosticOutcome::TimedOut,
                duration_ms: 125,
                error: None,
            },
        });

        assert_eq!(mapped.schema, "rootlight.web-quick-diagnostics/1");
        assert_eq!(mapped.overall_status, "degraded");
        assert_eq!(mapped.duration_ms, 125);
        assert_eq!(mapped.checks.len(), 1);
        assert_eq!(mapped.checks[0].name, "catalog");
        assert_eq!(mapped.checks[0].outcome, "timed_out");
    }

    #[test]
    fn support_download_is_local_attachment_with_digest() {
        let archive = b"PK\x03\x04support".to_vec();
        let digest = sha2::Sha256::digest(&archive).into();
        let response = support_download(SupportArtifact {
            archive: archive.clone(),
            sha256: digest,
        })
        .expect("support response builds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/zip"))
        );
        assert_eq!(
            response.headers().get(CONTENT_LENGTH),
            Some(&HeaderValue::from_str(&archive.len().to_string()).expect("length header"))
        );
        assert!(response.headers().contains_key("x-rootlight-sha256"));
    }
}
