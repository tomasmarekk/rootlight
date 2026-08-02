//! Exact loopback request validation and browser security response headers.

use axum::{
    extract::{Request, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
        header::{HOST, ORIGIN},
    },
    middleware::Next,
    response::{IntoResponse, Response},
};

const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");
const CONTENT_SECURITY_POLICY: HeaderName = HeaderName::from_static("content-security-policy");
const CROSS_ORIGIN_OPENER_POLICY: HeaderName =
    HeaderName::from_static("cross-origin-opener-policy");
const CROSS_ORIGIN_RESOURCE_POLICY: HeaderName =
    HeaderName::from_static("cross-origin-resource-policy");
const PERMISSIONS_POLICY: HeaderName = HeaderName::from_static("permissions-policy");

#[derive(Clone)]
pub(crate) struct SecurityPolicy {
    authority: String,
    origin: String,
}

impl SecurityPolicy {
    pub(crate) fn loopback(port: u16) -> Self {
        let authority = format!("127.0.0.1:{port}");
        let origin = format!("http://{authority}");
        Self { authority, origin }
    }

    pub(crate) fn origin(&self) -> &str {
        &self.origin
    }
}

pub(crate) async fn enforce(
    State(policy): State<SecurityPolicy>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let api = request.uri().path().starts_with("/api/");
    let accepted = host_is_exact(headers, &policy.authority)
        && if api {
            api_request_is_same_origin(request.method(), headers, &policy.origin)
        } else {
            navigation_or_asset_is_same_origin(headers)
        };
    if !accepted {
        return rejection(StatusCode::FORBIDDEN);
    }

    let mut response = next.run(request).await;
    apply_security_headers(response.headers_mut());
    response
}

fn is_mutation(method: &Method) -> bool {
    !matches!(method, &Method::GET | &Method::HEAD)
}

fn api_request_is_same_origin(method: &Method, headers: &HeaderMap, origin: &str) -> bool {
    let origin_matches = origin_is_exact(headers, origin);
    let fetch_site_matches = fetch_metadata_is_same_origin(headers);
    if is_mutation(method) {
        origin_matches && fetch_site_matches
    } else {
        origin_is_absent_or_exact(headers, origin)
            && fetch_metadata_is_absent_or_same_origin(headers)
            && (origin_matches || fetch_site_matches)
    }
}

fn navigation_or_asset_is_same_origin(headers: &HeaderMap) -> bool {
    headers
        .get(&SEC_FETCH_SITE)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| matches!(value, "same-origin" | "none"))
}

fn host_is_exact(headers: &HeaderMap, authority: &str) -> bool {
    headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == authority)
}

fn origin_is_exact(headers: &HeaderMap, origin: &str) -> bool {
    headers
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == origin)
}

fn origin_is_absent_or_exact(headers: &HeaderMap, origin: &str) -> bool {
    headers
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value == origin)
}

fn fetch_metadata_is_same_origin(headers: &HeaderMap) -> bool {
    headers
        .get(&SEC_FETCH_SITE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "same-origin")
}

fn fetch_metadata_is_absent_or_same_origin(headers: &HeaderMap) -> bool {
    headers
        .get(&SEC_FETCH_SITE)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value == "same-origin")
}

fn apply_security_headers(headers: &mut HeaderMap) {
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
             connect-src 'self'; font-src 'self'; worker-src 'self'; base-uri 'none'; \
             form-action 'none'; frame-ancestors 'none'; object-src 'none'",
        ),
    );
    headers.insert(
        CROSS_ORIGIN_OPENER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        CROSS_ORIGIN_RESOURCE_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        axum::http::header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        PERMISSIONS_POLICY,
        HeaderValue::from_static(
            "bluetooth=(), camera=(), clipboard-read=(), display-capture=(), geolocation=(), \
             microphone=(), payment=(), serial=(), usb=()",
        ),
    );
}

fn rejection(status: StatusCode) -> Response {
    let mut response = (
        status,
        [(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        )],
    )
        .into_response();
    apply_security_headers(response.headers_mut());
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_loopback_authority_and_origin_are_required() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("127.0.0.1:43127"));
        headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:43127"));
        headers.insert(SEC_FETCH_SITE, HeaderValue::from_static("same-origin"));

        assert!(host_is_exact(&headers, "127.0.0.1:43127"));
        assert!(origin_is_exact(&headers, "http://127.0.0.1:43127"));
        assert!(fetch_metadata_is_same_origin(&headers));
        assert!(api_request_is_same_origin(
            &Method::GET,
            &headers,
            "http://127.0.0.1:43127"
        ));
        headers.insert(HOST, HeaderValue::from_static("localhost:43127"));
        assert!(!host_is_exact(&headers, "127.0.0.1:43127"));
        headers.insert(SEC_FETCH_SITE, HeaderValue::from_static("cross-site"));
        assert!(!api_request_is_same_origin(
            &Method::GET,
            &headers,
            "http://127.0.0.1:43127"
        ));
    }
}
