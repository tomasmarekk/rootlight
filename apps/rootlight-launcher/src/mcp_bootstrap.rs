//! Conservative initialization-only MCP fast path for the Windows launcher.
//!
//! The stable launcher intentionally has no internal workspace dependencies.
//! This module therefore accepts only a strict, common subset of `initialize`.
//! Any extension or ambiguous input falls back to the full versioned payload.

use std::ffi::OsStr;

use serde::{Deserialize, Serialize};
use serde_json::Number;
use thiserror::Error;

const JSON_RPC_VERSION: &str = "2.0";
const MCP_SPECIFICATION_DATE: &str = "2025-11-25";
const MAX_REQUEST_ID_BYTES: usize = 4_096;
const MAX_IMPLEMENTATION_NAME_BYTES: usize = 256;
const MAX_IMPLEMENTATION_VERSION_BYTES: usize = 256;
const MAX_IMPLEMENTATION_TITLE_BYTES: usize = 512;
const MAX_IMPLEMENTATION_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_IMPLEMENTATION_ICONS: usize = 16;
const MAX_ICON_SOURCE_BYTES: usize = 4 * 1024;
const MAX_ICON_MIME_BYTES: usize = 256;
const MAX_ICON_SIZES: usize = 32;
const MAX_ICON_SIZE_BYTES: usize = 64;
const MAX_WEBSITE_BYTES: usize = 4 * 1024;

pub(super) const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;
pub(super) const DEFAULT_MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Failure while preparing the launcher's bounded initialization response.
#[derive(Debug, Error)]
pub(super) enum BootstrapInitializeError {
    /// The selected payload version is not a canonical bounded value.
    #[error("invalid MCP bootstrap server version")]
    InvalidServerVersion,
    /// The fixed response exceeds its configured byte ceiling.
    #[error("MCP bootstrap response exceeds its byte limit")]
    ResponseTooLarge,
    /// A bounded allocation could not be reserved.
    #[error("MCP bootstrap memory is unavailable")]
    MemoryUnavailable,
    /// The fixed response could not be serialized.
    #[error("MCP bootstrap response serialization failed")]
    Serialization(#[source] serde_json::Error),
}

pub(super) struct BootstrapInitialize {
    response: Vec<u8>,
}

impl BootstrapInitialize {
    pub(super) fn response(&self) -> &[u8] {
        &self.response
    }
}

/// Returns whether both optional environment overrides are valid profile names.
///
/// Profile negotiation does not alter the initialization response. The full
/// payload repeats the authoritative negotiation before it serves any tools.
pub(super) fn exposure_profile_policy_is_valid(
    ceiling: Option<&OsStr>,
    requested: Option<&OsStr>,
) -> bool {
    profile_override_is_valid(ceiling) && profile_override_is_valid(requested)
}

fn profile_override_is_valid(value: Option<&OsStr>) -> bool {
    let Some(value) = value else {
        return true;
    };
    value.is_empty()
        || value
            .to_str()
            .is_some_and(|value| matches!(value, "scout" | "analysis" | "developer"))
}

/// Prepares a frozen response for an unambiguous common `initialize` request.
///
/// `Ok(None)` deliberately falls back to the full payload. In particular, the
/// fast path does not interpret open-ended client capabilities or metadata.
pub(super) fn bootstrap_initialize(
    frame: &[u8],
    server_version: &str,
) -> Result<Option<BootstrapInitialize>, BootstrapInitializeError> {
    if !valid_server_version(server_version) {
        return Err(BootstrapInitializeError::InvalidServerVersion);
    }
    if frame.len() > DEFAULT_MAX_FRAME_BYTES {
        return Ok(None);
    }
    let request = match serde_json::from_slice::<InitializeRequest>(frame) {
        Ok(request) => request,
        Err(_) => return Ok(None),
    };
    if !request.is_valid() {
        return Ok(None);
    }

    let result = InitializeResult {
        protocol_version: MCP_SPECIFICATION_DATE,
        capabilities: ServerCapabilities {
            tools: ToolsCapability {
                list_changed: false,
            },
        },
        server_info: ServerImplementation {
            name: "rootlight",
            title: "Rootlight",
            version: server_version,
            description: "Local-first repository intelligence MCP bridge",
        },
    };
    let mut response = serde_json::to_vec(&ResultResponse {
        jsonrpc: JSON_RPC_VERSION,
        id: &request.id,
        result: &result,
    })
    .map_err(BootstrapInitializeError::Serialization)?;
    let required = response
        .len()
        .checked_add(1)
        .ok_or(BootstrapInitializeError::ResponseTooLarge)?;
    if required > DEFAULT_MAX_RESPONSE_BYTES {
        return Err(BootstrapInitializeError::ResponseTooLarge);
    }
    response
        .try_reserve_exact(1)
        .map_err(|_| BootstrapInitializeError::MemoryUnavailable)?;
    response.push(b'\n');
    Ok(Some(BootstrapInitialize { response }))
}

fn valid_server_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= MAX_IMPLEMENTATION_VERSION_BYTES
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitializeRequest {
    jsonrpc: String,
    id: RequestId,
    method: String,
    params: InitializeParams,
}

impl InitializeRequest {
    fn is_valid(&self) -> bool {
        self.jsonrpc == JSON_RPC_VERSION
            && self.method == "initialize"
            && self.id.is_valid()
            && self.params.is_valid()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum RequestId {
    Number(Number),
    String(String),
}

impl RequestId {
    fn is_valid(&self) -> bool {
        match self {
            Self::Number(number) => number.to_string().len() <= MAX_REQUEST_ID_BYTES,
            Self::String(value) => value.len() <= MAX_REQUEST_ID_BYTES,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InitializeParams {
    protocol_version: String,
    capabilities: EmptyCapabilities,
    client_info: ClientImplementation,
    initialization_options: Option<InitializationOptions>,
}

impl InitializeParams {
    fn is_valid(&self) -> bool {
        !self.protocol_version.is_empty()
            && self.protocol_version.len() <= MAX_IMPLEMENTATION_VERSION_BYTES
            && self.capabilities.is_valid()
            && self.client_info.is_valid()
            && self
                .initialization_options
                .as_ref()
                .is_none_or(InitializationOptions::is_valid)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyCapabilities {}

impl EmptyCapabilities {
    const fn is_valid(&self) -> bool {
        true
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientImplementation {
    name: String,
    version: String,
    title: Option<String>,
    description: Option<String>,
    icons: Option<Vec<ClientIcon>>,
    website_url: Option<String>,
}

impl ClientImplementation {
    fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= MAX_IMPLEMENTATION_NAME_BYTES
            && !self.version.is_empty()
            && self.version.len() <= MAX_IMPLEMENTATION_VERSION_BYTES
            && optional_string_is_bounded(self.title.as_deref(), MAX_IMPLEMENTATION_TITLE_BYTES)
            && optional_string_is_bounded(
                self.description.as_deref(),
                MAX_IMPLEMENTATION_DESCRIPTION_BYTES,
            )
            && optional_string_is_bounded(self.website_url.as_deref(), MAX_WEBSITE_BYTES)
            && self.icons.as_ref().is_none_or(|icons| {
                icons.len() <= MAX_IMPLEMENTATION_ICONS && icons.iter().all(ClientIcon::is_valid)
            })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientIcon {
    src: String,
    mime_type: Option<String>,
    sizes: Option<Vec<String>>,
    theme: Option<String>,
}

impl ClientIcon {
    fn is_valid(&self) -> bool {
        !self.src.is_empty()
            && self.src.len() <= MAX_ICON_SOURCE_BYTES
            && optional_string_is_bounded(self.mime_type.as_deref(), MAX_ICON_MIME_BYTES)
            && self
                .theme
                .as_deref()
                .is_none_or(|theme| matches!(theme, "light" | "dark"))
            && self.sizes.as_ref().is_none_or(|sizes| {
                sizes.len() <= MAX_ICON_SIZES
                    && sizes
                        .iter()
                        .all(|size| !size.is_empty() && size.len() <= MAX_ICON_SIZE_BYTES)
            })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitializationOptions {
    rootlight_exposure_profile: String,
}

impl InitializationOptions {
    fn is_valid(&self) -> bool {
        matches!(
            self.rootlight_exposure_profile.as_str(),
            "scout" | "analysis" | "developer"
        )
    }
}

fn optional_string_is_bounded(value: Option<&str>, maximum: usize) -> bool {
    value.is_none_or(|value| value.len() <= maximum)
}

#[derive(Serialize)]
struct ResultResponse<'a, I, T> {
    jsonrpc: &'static str,
    id: &'a I,
    result: &'a T,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeResult<'a> {
    protocol_version: &'a str,
    capabilities: ServerCapabilities,
    server_info: ServerImplementation<'a>,
}

#[derive(Serialize)]
struct ServerCapabilities {
    tools: ToolsCapability,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolsCapability {
    list_changed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerImplementation<'a> {
    name: &'a str,
    title: &'a str,
    version: &'a str,
    description: &'a str,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const INITIALIZE: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1.0"}}}"#;

    #[test]
    fn common_initialize_has_the_frozen_response() {
        let response = bootstrap_initialize(INITIALIZE, "9.8.7-alpha.6")
            .expect("response is encoded")
            .expect("request uses the fast path");

        assert_eq!(
            serde_json::from_slice::<Value>(response.response()).expect("response is JSON"),
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": MCP_SPECIFICATION_DATE,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {
                        "name": "rootlight",
                        "title": "Rootlight",
                        "version": "9.8.7-alpha.6",
                        "description": "Local-first repository intelligence MCP bridge"
                    }
                }
            })
        );
    }

    #[test]
    fn extended_or_ambiguous_requests_fall_back() {
        let extended = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{"roots":{}},"clientInfo":{"name":"fixture","version":"1.0"}}}"#;
        let duplicate = br#"{"jsonrpc":"2.0","id":1,"id":2,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1.0"}}}"#;

        assert!(
            bootstrap_initialize(extended, "1.0.0")
                .expect("fallback is not an error")
                .is_none()
        );
        assert!(
            bootstrap_initialize(duplicate, "1.0.0")
                .expect("fallback is not an error")
                .is_none()
        );
    }

    #[test]
    fn profile_option_uses_the_same_nested_wire_name_as_the_full_server() {
        let snake_case = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1.0"},"initializationOptions":{"rootlight_exposure_profile":"scout"}}}"#;
        let camel_case = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1.0"},"initializationOptions":{"rootlightExposureProfile":"scout"}}}"#;

        assert!(
            bootstrap_initialize(snake_case, "1.0.0")
                .expect("canonical profile option is processed")
                .is_some()
        );
        assert!(
            bootstrap_initialize(camel_case, "1.0.0")
                .expect("noncanonical profile option falls back")
                .is_none()
        );
    }
}
