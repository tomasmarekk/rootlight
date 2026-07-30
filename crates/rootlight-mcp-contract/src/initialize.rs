//! Frozen MCP initialization contract shared by the server and stable launcher.

use std::ffi::OsStr;

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::{
    ExposureProfile, MCP_SPECIFICATION_DATE,
    json::{JsonLimits, MAX_SUPPORTED_JSON_DEPTH, ParseFailure, parse_bounded},
};

const JSON_RPC_VERSION: &str = "2.0";
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

/// Default maximum bytes in one newline-delimited MCP message.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Default maximum bytes in one encoded MCP response, including its newline.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Default maximum JSON object or array nesting below the top-level value.
pub const DEFAULT_MAX_JSON_DEPTH: usize = 32;
/// Default maximum UTF-8 bytes in one JSON string or object key.
pub const DEFAULT_MAX_STRING_BYTES: usize = 64 * 1024;
/// Default maximum raw properties accepted in one JSON object.
pub const DEFAULT_MAX_OBJECT_PROPERTIES: usize = 128;
/// Default maximum values accepted in one JSON array.
pub const DEFAULT_MAX_ARRAY_ITEMS: usize = 256;
/// Default maximum aggregate JSON values accepted in one message.
pub const DEFAULT_MAX_JSON_NODES: usize = 16 * 1024;

/// Bounded input and output limits for initialization-only processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapLimits {
    max_frame_bytes: usize,
    max_response_bytes: usize,
    json: JsonLimits,
}

impl BootstrapLimits {
    /// Creates initialization limits after their caller has validated the
    /// individual values against the server's supported ceilings.
    #[must_use]
    pub const fn new(
        max_frame_bytes: usize,
        max_response_bytes: usize,
        max_json_depth: usize,
        max_string_bytes: usize,
        max_object_properties: usize,
        max_array_items: usize,
        max_nodes: usize,
    ) -> Self {
        Self {
            max_frame_bytes,
            max_response_bytes,
            json: JsonLimits {
                max_depth: max_json_depth,
                max_string_bytes,
                max_object_properties,
                max_array_items,
                max_nodes,
            },
        }
    }

    /// Returns the maximum accepted frame bytes.
    #[must_use]
    pub const fn max_frame_bytes(self) -> usize {
        self.max_frame_bytes
    }

    fn is_valid(self) -> bool {
        self.max_frame_bytes > 0
            && self.max_response_bytes > 0
            && self.json.max_depth > 0
            && self.json.max_depth <= MAX_SUPPORTED_JSON_DEPTH
            && self.json.max_string_bytes > 0
            && self.json.max_object_properties > 0
            && self.json.max_array_items > 0
            && self.json.max_nodes > 0
    }
}

impl Default for BootstrapLimits {
    fn default() -> Self {
        Self::new(
            DEFAULT_MAX_FRAME_BYTES,
            DEFAULT_MAX_RESPONSE_BYTES,
            DEFAULT_MAX_JSON_DEPTH,
            DEFAULT_MAX_STRING_BYTES,
            DEFAULT_MAX_OBJECT_PROPERTIES,
            DEFAULT_MAX_ARRAY_ITEMS,
            DEFAULT_MAX_JSON_NODES,
        )
    }
}

/// Invalid server exposure-profile environment policy.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("invalid MCP exposure-profile policy")]
pub struct ExposureProfilePolicyError;

/// Resolves the server exposure-profile ceiling and default from environment
/// override values.
///
/// Empty values use their defaults. A requested default is always clamped to
/// the ceiling so configuration cannot raise a client's privilege level.
///
/// # Errors
///
/// Returns [`ExposureProfilePolicyError`] when either non-empty value is not
/// UTF-8 or does not name a supported exposure profile.
pub fn resolve_exposure_profile_policy(
    ceiling: Option<&OsStr>,
    requested: Option<&OsStr>,
) -> Result<(ExposureProfile, ExposureProfile), ExposureProfilePolicyError> {
    let ceiling = parse_profile_override(ceiling, ExposureProfile::Developer)?;
    let requested = parse_profile_override(requested, ceiling)?;
    Ok((ceiling, requested.clamped_to(ceiling)))
}

fn parse_profile_override(
    raw: Option<&OsStr>,
    default: ExposureProfile,
) -> Result<ExposureProfile, ExposureProfilePolicyError> {
    let Some(raw) = raw else {
        return Ok(default);
    };
    if raw.is_empty() {
        return Ok(default);
    }
    raw.to_str()
        .and_then(ExposureProfile::from_name)
        .ok_or(ExposureProfilePolicyError)
}

/// A validated handler-compatible `initialize` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapInitialize {
    response: Vec<u8>,
    negotiated_profile: ExposureProfile,
}

impl BootstrapInitialize {
    /// Returns the exact newline-terminated JSON-RPC response frame.
    #[must_use]
    pub fn response(&self) -> &[u8] {
        &self.response
    }

    /// Returns the exposure profile selected for the initialized session.
    #[must_use]
    pub const fn negotiated_profile(&self) -> ExposureProfile {
        self.negotiated_profile
    }
}

/// Failure while preparing a bounded initialization response.
#[derive(Debug, Error)]
pub enum BootstrapInitializeError {
    /// The selected installed release version is not a canonical bounded value.
    #[error("invalid MCP bootstrap server version")]
    InvalidServerVersion,
    /// One or more bounded transport limits are invalid.
    #[error("invalid MCP bootstrap limits")]
    InvalidLimits,
    /// A bounded allocation could not be reserved.
    #[error("MCP bootstrap memory is unavailable")]
    MemoryUnavailable,
    /// The fixed response exceeds its configured byte ceiling.
    #[error("MCP bootstrap response exceeds its byte limit")]
    ResponseTooLarge,
    /// The fixed response could not be serialized.
    #[error("MCP bootstrap response serialization failed")]
    Serialization(#[source] serde_json::Error),
}

/// Prepares the exact response for an eligible first `initialize` request.
///
/// `Ok(None)` means the frame must be handled by the full MCP session. The
/// function uses the same bounded parser, parameter validator, profile
/// negotiation, and response encoder as the full server.
///
/// # Errors
///
/// Returns [`BootstrapInitializeError`] for invalid server configuration,
/// bounded allocation failure, response overflow, or serialization failure.
pub fn bootstrap_initialize(
    frame: &[u8],
    ceiling: ExposureProfile,
    default_profile: ExposureProfile,
    server_version: &str,
    limits: BootstrapLimits,
) -> Result<Option<BootstrapInitialize>, BootstrapInitializeError> {
    if !valid_server_version(server_version) {
        return Err(BootstrapInitializeError::InvalidServerVersion);
    }
    if !limits.is_valid() {
        return Err(BootstrapInitializeError::InvalidLimits);
    }
    if frame.len() > limits.max_frame_bytes {
        return Ok(None);
    }
    let parsed = match parse_bounded(frame, limits.json) {
        Ok(value) => value,
        Err(ParseFailure::MemoryUnavailable) => {
            return Err(BootstrapInitializeError::MemoryUnavailable);
        }
        Err(ParseFailure::Malformed | ParseFailure::Rejected(_)) => return Ok(None),
    };
    let Value::Object(mut object) = parsed else {
        return Ok(None);
    };
    let Some(id) = object.remove("id").filter(valid_request_id) else {
        return Ok(None);
    };
    let jsonrpc = object.remove("jsonrpc");
    let method = object.remove("method");
    let params = object.remove("params");
    if !object.is_empty()
        || !matches!(jsonrpc, Some(Value::String(version)) if version == JSON_RPC_VERSION)
        || !matches!(method, Some(Value::String(method)) if method == "initialize")
    {
        return Ok(None);
    }
    let Some(negotiated_profile) =
        negotiate_initialize_profile(params.as_ref(), ceiling, default_profile)
    else {
        return Ok(None);
    };
    let response =
        encode_initialize_response(&id, true, server_version, limits.max_response_bytes)?;
    Ok(Some(BootstrapInitialize {
        response,
        negotiated_profile,
    }))
}

/// Validates initialize parameters and resolves the resulting exposure profile.
///
/// `None` means the parameters are not accepted by the frozen MCP contract.
#[must_use]
pub fn negotiate_initialize_profile(
    params: Option<&Value>,
    ceiling: ExposureProfile,
    default_profile: ExposureProfile,
) -> Option<ExposureProfile> {
    if !initialize_params_are_valid(params) {
        return None;
    }
    Some(
        requested_exposure_profile(params)
            .unwrap_or(default_profile)
            .clamped_to(ceiling),
    )
}

/// Encodes the frozen initialize response used by the full server and launcher.
///
/// # Errors
///
/// Returns [`BootstrapInitializeError`] for an invalid server version,
/// serialization failure, response overflow, or bounded allocation failure.
pub fn encode_initialize_response(
    id: &impl Serialize,
    tools: bool,
    server_version: &str,
    maximum_bytes: usize,
) -> Result<Vec<u8>, BootstrapInitializeError> {
    if !valid_server_version(server_version) {
        return Err(BootstrapInitializeError::InvalidServerVersion);
    }
    let result = InitializeResult {
        protocol_version: MCP_SPECIFICATION_DATE,
        capabilities: ServerCapabilities {
            tools: tools.then_some(ToolsCapability {
                list_changed: false,
            }),
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
        id,
        result: &result,
    })
    .map_err(BootstrapInitializeError::Serialization)?;
    let required = response
        .len()
        .checked_add(1)
        .ok_or(BootstrapInitializeError::ResponseTooLarge)?;
    if required > maximum_bytes {
        return Err(BootstrapInitializeError::ResponseTooLarge);
    }
    response
        .try_reserve_exact(1)
        .map_err(|_| BootstrapInitializeError::MemoryUnavailable)?;
    response.push(b'\n');
    Ok(response)
}

fn valid_request_id(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.to_string().len() <= MAX_REQUEST_ID_BYTES,
        Value::String(value) => value.len() <= MAX_REQUEST_ID_BYTES,
        _ => false,
    }
}

fn valid_server_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= MAX_IMPLEMENTATION_VERSION_BYTES
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

fn initialize_params_are_valid(params: Option<&Value>) -> bool {
    let Some(Value::Object(params)) = params else {
        return false;
    };
    if params.keys().any(|key| {
        !matches!(
            key.as_str(),
            "_meta" | "protocolVersion" | "capabilities" | "clientInfo" | "initializationOptions"
        )
    }) || params
        .get("_meta")
        .is_some_and(|meta| !request_meta_is_valid(meta))
    {
        return false;
    }
    let Some(Value::String(protocol_version)) = params.get("protocolVersion") else {
        return false;
    };
    !protocol_version.is_empty()
        && protocol_version.len() <= MAX_IMPLEMENTATION_VERSION_BYTES
        && client_capabilities_are_valid(params.get("capabilities"))
        && client_implementation_is_valid(params.get("clientInfo"))
        && params
            .get("initializationOptions")
            .is_none_or(initialization_options_are_valid)
}

fn request_meta_is_valid(value: &Value) -> bool {
    let Some(meta) = value.as_object() else {
        return false;
    };
    meta.get("progressToken")
        .is_none_or(|token| token.is_string() || token.is_number())
}

fn initialization_options_are_valid(value: &Value) -> bool {
    let Some(options) = value.as_object() else {
        return false;
    };
    options.len() == 1
        && options
            .get("rootlight_exposure_profile")
            .and_then(Value::as_str)
            .and_then(ExposureProfile::from_name)
            .is_some()
}

fn requested_exposure_profile(params: Option<&Value>) -> Option<ExposureProfile> {
    params?
        .get("initializationOptions")?
        .get("rootlight_exposure_profile")?
        .as_str()
        .and_then(ExposureProfile::from_name)
}

fn client_capabilities_are_valid(capabilities: Option<&Value>) -> bool {
    let Some(Value::Object(capabilities)) = capabilities else {
        return false;
    };
    capabilities
        .iter()
        .all(|(name, value)| match name.as_str() {
            "experimental" => {
                matches!(value, Value::Object(values) if values.values().all(Value::is_object))
            }
            "roots" => object_has_typed_fields(value, &[("listChanged", JsonKind::Boolean)]),
            "sampling" => object_has_typed_fields(
                value,
                &[("context", JsonKind::Object), ("tools", JsonKind::Object)],
            ),
            "elicitation" => object_has_typed_fields(
                value,
                &[("form", JsonKind::Object), ("url", JsonKind::Object)],
            ),
            "tasks" => tasks_capability_is_valid(value),
            _ => true,
        })
}

fn tasks_capability_is_valid(value: &Value) -> bool {
    let Some(tasks) = value.as_object() else {
        return false;
    };
    if tasks.get("list").is_some_and(|value| !value.is_object())
        || tasks.get("cancel").is_some_and(|value| !value.is_object())
    {
        return false;
    }
    let Some(requests) = tasks.get("requests") else {
        return true;
    };
    let Some(requests) = requests.as_object() else {
        return false;
    };
    requests
        .get("sampling")
        .is_none_or(|value| object_has_typed_fields(value, &[("createMessage", JsonKind::Object)]))
        && requests
            .get("elicitation")
            .is_none_or(|value| object_has_typed_fields(value, &[("create", JsonKind::Object)]))
}

fn client_implementation_is_valid(value: Option<&Value>) -> bool {
    let Some(Value::Object(implementation)) = value else {
        return false;
    };
    if implementation.keys().any(|key| {
        !matches!(
            key.as_str(),
            "name" | "title" | "version" | "description" | "icons" | "websiteUrl"
        )
    }) {
        return false;
    }
    let Some(Value::String(name)) = implementation.get("name") else {
        return false;
    };
    let Some(Value::String(version)) = implementation.get("version") else {
        return false;
    };
    if name.is_empty()
        || name.len() > MAX_IMPLEMENTATION_NAME_BYTES
        || version.is_empty()
        || version.len() > MAX_IMPLEMENTATION_VERSION_BYTES
        || !optional_bounded_string(implementation.get("title"), MAX_IMPLEMENTATION_TITLE_BYTES)
        || !optional_bounded_string(
            implementation.get("description"),
            MAX_IMPLEMENTATION_DESCRIPTION_BYTES,
        )
        || !optional_bounded_string(implementation.get("websiteUrl"), MAX_WEBSITE_BYTES)
    {
        return false;
    }
    implementation.get("icons").is_none_or(|icons| {
        matches!(
            icons,
            Value::Array(icons)
                if icons.len() <= MAX_IMPLEMENTATION_ICONS
                    && icons.iter().all(client_icon_is_valid)
        )
    })
}

fn client_icon_is_valid(value: &Value) -> bool {
    let Some(icon) = value.as_object() else {
        return false;
    };
    if icon
        .keys()
        .any(|key| !matches!(key.as_str(), "src" | "mimeType" | "sizes" | "theme"))
    {
        return false;
    }
    let Some(Value::String(source)) = icon.get("src") else {
        return false;
    };
    if source.is_empty()
        || source.len() > MAX_ICON_SOURCE_BYTES
        || !optional_bounded_string(icon.get("mimeType"), MAX_ICON_MIME_BYTES)
        || !valid_icon_theme(icon.get("theme"))
    {
        return false;
    }
    icon.get("sizes").is_none_or(|sizes| {
        matches!(
            sizes,
            Value::Array(sizes)
                if sizes.len() <= MAX_ICON_SIZES
                    && sizes.iter().all(|size| {
                        matches!(
                            size,
                            Value::String(size)
                                if !size.is_empty() && size.len() <= MAX_ICON_SIZE_BYTES
                        )
                    })
        )
    })
}

fn valid_icon_theme(theme: Option<&Value>) -> bool {
    match theme {
        None => true,
        Some(Value::String(theme)) => matches!(theme.as_str(), "light" | "dark"),
        Some(_) => false,
    }
}

fn optional_bounded_string(value: Option<&Value>, maximum: usize) -> bool {
    match value {
        None => true,
        Some(Value::String(value)) => value.len() <= maximum,
        Some(_) => false,
    }
}

#[derive(Clone, Copy)]
enum JsonKind {
    Boolean,
    Object,
}

fn object_has_typed_fields(value: &Value, fields: &[(&str, JsonKind)]) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    fields.iter().all(|(name, kind)| {
        object.get(*name).is_none_or(|value| match kind {
            JsonKind::Boolean => value.is_boolean(),
            JsonKind::Object => value.is_object(),
        })
    })
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
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<ToolsCapability>,
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
    use serde_json::json;

    use super::*;

    #[test]
    fn frozen_response_includes_the_selected_payload_version() {
        let response = bootstrap_initialize(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1.0"}}}"#,
            ExposureProfile::Developer,
            ExposureProfile::Developer,
            "9.8.7-alpha.6",
            BootstrapLimits::default(),
        )
        .expect("initialize is encoded")
        .expect("initialize is eligible");

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
}
