//! Versioned, immutable configuration contracts for Rootlight.
//!
//! Callers supply bytes for explicit configuration layers. This crate never
//! reads ambient files, environment variables, credentials, or network state.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use rootlight_error::{DetailKey, ErrorCode, NextAction, PublicError, SafeLabel};
use rootlight_ids::{ContentHash, content_hash};
use serde::{Deserialize, Serialize};

/// The initial production configuration contract version.
pub const CONFIG_VERSION: ContractVersion = ContractVersion::new(1, 0);

/// A checked major/minor version for public contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContractVersion {
    major: u16,
    minor: u16,
}

impl ContractVersion {
    /// Creates a version from its numeric components.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Returns the major compatibility version.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Returns the additive minor version.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Parses canonical `major.minor` text without leading zeroes.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidVersion`] for malformed or noncanonical
    /// version text.
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        let (major, minor) = input.split_once('.').ok_or(ConfigError::InvalidVersion)?;
        if major.is_empty()
            || minor.is_empty()
            || major.starts_with('0') && major != "0"
            || minor.starts_with('0') && minor != "0"
            || minor.contains('.')
        {
            return Err(ConfigError::InvalidVersion);
        }
        let parsed = Self {
            major: major.parse().map_err(|_| ConfigError::InvalidVersion)?,
            minor: minor.parse().map_err(|_| ConfigError::InvalidVersion)?,
        };
        if parsed.to_string() != input {
            return Err(ConfigError::InvalidVersion);
        }
        Ok(parsed)
    }

    /// Ensures the configuration belongs to the supported production major.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::UnsupportedMajor`] when the major differs from
    /// [`CONFIG_VERSION`].
    pub fn require_supported(self) -> Result<Self, ConfigError> {
        if self.major == CONFIG_VERSION.major {
            Ok(self)
        } else {
            Err(ConfigError::UnsupportedMajor { major: self.major })
        }
    }
}

impl std::fmt::Display for ContractVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for ContractVersion {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ContractVersion".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^1\\.(0|[1-9][0-9]*)$",
        })
    }
}

/// Authority of one configuration layer, ordered from strongest to weakest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    /// Compiled safe defaults.
    Defaults,
    /// Machine-wide administrative policy.
    System,
    /// Per-user configuration.
    User,
    /// Repository-local configuration.
    Repository,
    /// One operation's explicit override.
    Operation,
}

/// Closed network policy supported by the core configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// No inbound or outbound network behavior.
    Deny,
    /// Authenticated loopback transport only.
    Loopback,
}

/// Closed repository execution policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RepositoryExecutionPolicy {
    /// Repository-controlled programs are not executed.
    Deny,
    /// A separately consented operation may execute a fixed command.
    ExplicitConsent,
}

/// Closed in-process native plugin policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NativePluginPolicy {
    /// Native third-party plugins cannot load into the core process.
    Deny,
}

/// The initial analysis depth selected by configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AnalysisTier {
    /// Syntax and structural facts without compiler execution.
    Structural,
    /// Isolated deep analysis when capability and policy permit it.
    Deep,
}

/// Security configuration whose denials cannot be weakened by lower authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    /// Network access policy.
    pub network: NetworkPolicy,
    /// Repository code execution policy.
    pub repository_execution: RepositoryExecutionPolicy,
    /// Native plugin loading policy.
    pub in_process_native_plugins: NativePluginPolicy,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            network: NetworkPolicy::Deny,
            repository_execution: RepositoryExecutionPolicy::Deny,
            in_process_native_plugins: NativePluginPolicy::Deny,
        }
    }
}

/// Bounded resource configuration for contract-level operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ResourceConfig {
    /// Maximum source bytes one contract operation may request.
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 16_777_216)))]
    pub max_source_bytes: u64,
    /// Maximum result records one contract operation may return.
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 10_000)))]
    pub max_results: u32,
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            max_source_bytes: 65_536,
            max_results: 50,
        }
    }
}

/// Analysis defaults that do not widen security policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AnalysisConfig {
    /// Default requested analysis tier.
    pub default_tier: AnalysisTier,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            default_tier: AnalysisTier::Structural,
        }
    }
}

/// Metadata retained for an optional namespaced extension.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionSnapshot {
    namespace: String,
    version: ContractVersion,
    critical: bool,
    bytes: u64,
    digest: ContentHash,
}

impl ExtensionSnapshot {
    /// Returns the validated namespace.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Returns the extension version.
    #[must_use]
    pub const fn version(&self) -> ContractVersion {
        self.version
    }

    /// Reports whether an unknown extension must be rejected.
    #[must_use]
    pub const fn critical(&self) -> bool {
        self.critical
    }

    /// Returns the canonical extension byte count.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Returns the canonical extension digest.
    #[must_use]
    pub const fn digest(&self) -> ContentHash {
        self.digest
    }
}

impl std::fmt::Debug for ExtensionSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionSnapshot")
            .field("namespace", &self.namespace)
            .field("version", &self.version)
            .field("critical", &self.critical)
            .field("bytes", &self.bytes)
            .field("digest", &self.digest)
            .finish()
    }
}

/// Immutable effective configuration with provenance and canonical identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSnapshot {
    version: ContractVersion,
    security: SecurityConfig,
    resources: ResourceConfig,
    analysis: AnalysisConfig,
    extensions: BTreeMap<String, ExtensionSnapshot>,
    provenance: BTreeMap<String, ConfigSource>,
    canonical: Vec<u8>,
    hash: ContentHash,
}

impl ConfigSnapshot {
    /// Resolves supplied layers in authority order into one immutable snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for malformed TOML, unsupported versions,
    /// unknown fields, invalid bounds, or unknown critical extensions.
    pub fn resolve(layers: &[ConfigLayer<'_>]) -> Result<Self, ConfigError> {
        let mut state = EffectiveConfig::defaults();
        let mut ordered = layers.to_vec();
        ordered.sort_by_key(|layer| layer.source);
        for layer in ordered {
            let parsed: WireConfig =
                toml::from_str(layer.contents).map_err(|source| ConfigError::Parse { source })?;
            parsed.version.require_supported()?;
            state.apply(layer.source, parsed)?;
        }
        state.finish()
    }

    /// Returns the effective contract version.
    #[must_use]
    pub const fn version(&self) -> ContractVersion {
        self.version
    }

    /// Returns the effective security policy.
    #[must_use]
    pub const fn security(&self) -> SecurityConfig {
        self.security
    }

    /// Returns the effective resource bounds.
    #[must_use]
    pub const fn resources(&self) -> ResourceConfig {
        self.resources
    }

    /// Returns the effective analysis defaults.
    #[must_use]
    pub const fn analysis(&self) -> AnalysisConfig {
        self.analysis
    }

    /// Returns preserved optional extension metadata without exposing payloads.
    #[must_use]
    pub const fn extensions(&self) -> &BTreeMap<String, ExtensionSnapshot> {
        &self.extensions
    }

    /// Returns per-field configuration provenance.
    #[must_use]
    pub const fn provenance(&self) -> &BTreeMap<String, ConfigSource> {
        &self.provenance
    }

    /// Returns the canonical serialized configuration bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }

    /// Returns the hash of the canonical effective configuration.
    #[must_use]
    pub const fn hash(&self) -> ContentHash {
        self.hash
    }
}

/// One explicit configuration source supplied by the caller.
#[derive(Debug, Clone, Copy)]
pub struct ConfigLayer<'a> {
    /// Authority of the layer.
    pub source: ConfigSource,
    /// UTF-8 TOML contents of the layer.
    pub contents: &'a str,
}

#[derive(Debug, Clone)]
struct EffectiveConfig {
    security: SecurityConfig,
    resources: ResourceConfig,
    analysis: AnalysisConfig,
    extensions: BTreeMap<String, ExtensionSnapshot>,
    provenance: BTreeMap<String, ConfigSource>,
}

impl EffectiveConfig {
    fn defaults() -> Self {
        let mut provenance = BTreeMap::new();
        for field in [
            "security.network",
            "security.repository_execution",
            "security.in_process_native_plugins",
            "resources.max_source_bytes",
            "resources.max_results",
            "analysis.default_tier",
        ] {
            provenance.insert(field.to_owned(), ConfigSource::Defaults);
        }
        Self {
            security: SecurityConfig::default(),
            resources: ResourceConfig::default(),
            analysis: AnalysisConfig::default(),
            extensions: BTreeMap::new(),
            provenance,
        }
    }

    fn apply(&mut self, source: ConfigSource, wire: WireConfig) -> Result<(), ConfigError> {
        if let Some(security) = wire.security {
            if let Some(network) = security.network {
                self.security.network = merge_network(self.security.network, network);
                self.provenance
                    .insert("security.network".to_owned(), source);
            }
            if let Some(execution) = security.repository_execution {
                self.security.repository_execution =
                    merge_execution(self.security.repository_execution, execution);
                self.provenance
                    .insert("security.repository_execution".to_owned(), source);
            }
            if security.in_process_native_plugins.is_some() {
                self.security.in_process_native_plugins = NativePluginPolicy::Deny;
                self.provenance
                    .insert("security.in_process_native_plugins".to_owned(), source);
            }
        }
        if let Some(resources) = wire.resources {
            if let Some(max_source_bytes) = resources.max_source_bytes {
                if !(1..=16 * 1024 * 1024).contains(&max_source_bytes) {
                    return Err(ConfigError::ResourceLimitOutOfRange {
                        field: "max_source_bytes",
                    });
                }
                self.resources.max_source_bytes = max_source_bytes;
                self.provenance
                    .insert("resources.max_source_bytes".to_owned(), source);
            }
            if let Some(max_results) = resources.max_results {
                if !(1..=10_000).contains(&max_results) {
                    return Err(ConfigError::ResourceLimitOutOfRange {
                        field: "max_results",
                    });
                }
                self.resources.max_results = max_results;
                self.provenance
                    .insert("resources.max_results".to_owned(), source);
            }
        }
        if let Some(analysis) = wire.analysis
            && let Some(default_tier) = analysis.default_tier
        {
            self.analysis.default_tier = default_tier;
            self.provenance
                .insert("analysis.default_tier".to_owned(), source);
        }
        for (namespace, extension) in wire.extensions {
            validate_namespace(&namespace)?;
            if extension.critical {
                return Err(ConfigError::UnknownCriticalExtension { namespace });
            }
            extension.version.require_supported()?;
            let canonical = canonical_extension(&extension.data)?;
            let bytes =
                u64::try_from(canonical.len()).map_err(|_| ConfigError::ExtensionTooLarge)?;
            if bytes > 64 * 1024 {
                return Err(ConfigError::ExtensionTooLarge);
            }
            let snapshot = ExtensionSnapshot {
                namespace: namespace.clone(),
                version: extension.version,
                critical: false,
                bytes,
                digest: content_hash(&canonical),
            };
            self.extensions.insert(namespace.clone(), snapshot);
            self.provenance
                .insert(format!("extensions.{namespace}"), source);
        }
        Ok(())
    }

    fn finish(self) -> Result<ConfigSnapshot, ConfigError> {
        let canonical_wire = CanonicalConfig {
            version: CONFIG_VERSION,
            security: self.security,
            resources: self.resources,
            analysis: self.analysis,
            extensions: self.extensions.clone(),
        };
        let canonical = serde_json::to_vec(&canonical_wire)
            .map_err(|source| ConfigError::Canonicalize { source })?;
        let hash = content_hash(&canonical);
        Ok(ConfigSnapshot {
            version: CONFIG_VERSION,
            security: self.security,
            resources: self.resources,
            analysis: self.analysis,
            extensions: self.extensions,
            provenance: self.provenance,
            canonical,
            hash,
        })
    }
}

fn merge_network(current: NetworkPolicy, requested: NetworkPolicy) -> NetworkPolicy {
    if current == NetworkPolicy::Deny || requested == NetworkPolicy::Deny {
        NetworkPolicy::Deny
    } else {
        NetworkPolicy::Loopback
    }
}

fn merge_execution(
    current: RepositoryExecutionPolicy,
    requested: RepositoryExecutionPolicy,
) -> RepositoryExecutionPolicy {
    if current == RepositoryExecutionPolicy::Deny || requested == RepositoryExecutionPolicy::Deny {
        RepositoryExecutionPolicy::Deny
    } else {
        RepositoryExecutionPolicy::ExplicitConsent
    }
}

fn validate_namespace(namespace: &str) -> Result<(), ConfigError> {
    let valid = !namespace.is_empty()
        && namespace.len() <= 64
        && namespace.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        });
    if valid {
        Ok(())
    } else {
        Err(ConfigError::InvalidExtensionNamespace)
    }
}

fn canonical_extension(data: &toml::Value) -> Result<Vec<u8>, ConfigError> {
    fn canonicalize(value: &toml::Value) -> Result<serde_json::Value, ConfigError> {
        match value {
            toml::Value::String(value) => Ok(serde_json::Value::String(value.clone())),
            toml::Value::Integer(value) => Ok(serde_json::Value::Number((*value).into())),
            toml::Value::Boolean(value) => Ok(serde_json::Value::Bool(*value)),
            toml::Value::Array(values) => values
                .iter()
                .map(canonicalize)
                .collect::<Result<Vec<_>, _>>()
                .map(serde_json::Value::Array),
            toml::Value::Table(values) => values
                .iter()
                .map(|(key, value)| Ok((key.clone(), canonicalize(value)?)))
                .collect::<Result<serde_json::Map<_, _>, ConfigError>>()
                .map(serde_json::Value::Object),
            toml::Value::Float(_) | toml::Value::Datetime(_) => {
                Err(ConfigError::UnsupportedExtensionValue)
            }
        }
    }

    serde_json::to_vec(&canonicalize(data)?).map_err(|source| ConfigError::Canonicalize { source })
}

/// Namespaced extension map with a closed namespace policy.
#[cfg(feature = "schema")]
struct ExtensionMapSchema;

#[cfg(feature = "schema")]
impl schemars::JsonSchema for ExtensionMapSchema {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ExtensionMap".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let extension = generator.subschema_for::<WireExtension>();
        schemars::json_schema!({
            "type": "object",
            "propertyNames": {
                "pattern": "^[a-z0-9.-]{1,64}$"
            },
            "additionalProperties": extension
        })
    }
}

/// Strict external shape of the versioned configuration document.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(title = "Rootlight configuration 1.0"))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigDocumentSchema {
    /// Canonical contract version selected by the document.
    version: ContractVersion,
    /// Optional security policy overrides.
    #[serde(default)]
    security: Option<PartialSecurityConfig>,
    /// Optional resource limit overrides.
    #[serde(default)]
    resources: Option<PartialResourceConfig>,
    /// Optional analysis defaults.
    #[serde(default)]
    analysis: Option<PartialAnalysisConfig>,
    /// Namespaced extensions keyed by their stable namespace.
    #[serde(default)]
    #[cfg_attr(feature = "schema", schemars(with = "ExtensionMapSchema"))]
    extensions: BTreeMap<String, WireExtension>,
}

type WireConfig = ConfigDocumentSchema;

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct PartialSecurityConfig {
    network: Option<NetworkPolicy>,
    repository_execution: Option<RepositoryExecutionPolicy>,
    in_process_native_plugins: Option<NativePluginPolicy>,
}

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct PartialResourceConfig {
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 16_777_216)))]
    max_source_bytes: Option<u64>,
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 10_000)))]
    max_results: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct PartialAnalysisConfig {
    default_tier: Option<AnalysisTier>,
}

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct WireExtension {
    version: ContractVersion,
    critical: bool,
    #[serde(default = "empty_extension_data")]
    #[cfg_attr(feature = "schema", schemars(with = "serde_json::Value"))]
    data: toml::Value,
}

fn empty_extension_data() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

#[derive(Debug, Serialize)]
struct CanonicalConfig {
    version: ContractVersion,
    security: SecurityConfig,
    resources: ResourceConfig,
    analysis: AnalysisConfig,
    extensions: BTreeMap<String, ExtensionSnapshot>,
}

/// Configuration parsing and resolution failures.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// TOML was malformed or used an unknown field.
    #[error("failed to parse configuration")]
    Parse {
        /// Underlying TOML decoder error retained internally.
        #[source]
        source: toml::de::Error,
    },
    /// Contract version text was malformed or noncanonical.
    #[error("invalid configuration contract version")]
    InvalidVersion,
    /// The configuration uses an unsupported major version.
    #[error("unsupported configuration major version {major}")]
    UnsupportedMajor {
        /// Unsupported major component.
        major: u16,
    },
    /// A resource field was outside its declared hard bounds.
    #[error("configuration resource limit is out of range: {field}")]
    ResourceLimitOutOfRange {
        /// Stable source-free field name.
        field: &'static str,
    },
    /// Unknown critical extension data cannot be safely ignored.
    #[error("unknown critical configuration extension: {namespace}")]
    UnknownCriticalExtension {
        /// Validated extension namespace.
        namespace: String,
    },
    /// Extension namespace violated its safe allow-list.
    #[error("invalid configuration extension namespace")]
    InvalidExtensionNamespace,
    /// Extension data exceeded its bounded canonical representation.
    #[error("configuration extension exceeds its byte limit")]
    ExtensionTooLarge,
    /// Float and datetime extension values are not canonicalized in P0.
    #[error("configuration extension uses an unsupported value type")]
    UnsupportedExtensionValue,
    /// Canonical JSON serialization failed unexpectedly.
    #[error("failed to canonicalize configuration")]
    Canonicalize {
        /// Underlying serializer error retained internally.
        #[source]
        source: serde_json::Error,
    },
}

fn detail_key(value: &'static str) -> DetailKey {
    match DetailKey::parse(value) {
        Ok(key) => key,
        Err(_) => unreachable!("hard-coded detail keys satisfy the public allow-list"),
    }
}

fn safe_label_or_fallback(value: &str) -> SafeLabel {
    SafeLabel::parse(value).unwrap_or_else(|_| {
        SafeLabel::parse("invalid-extension")
            .unwrap_or_else(|_| unreachable!("hard-coded safe label satisfies the allow-list"))
    })
}

impl ConfigError {
    /// Converts the internal failure to a bounded source-redacted public error.
    #[must_use]
    pub fn to_public(&self) -> PublicError {
        let builder = match self {
            Self::UnsupportedMajor { major } => PublicError::builder(
                ErrorCode::ProtocolMismatch,
                "configuration major version is unsupported",
            )
            .detail(
                detail_key("major"),
                rootlight_error::PublicValue::Unsigned(u64::from(*major)),
            )
            .next_action(NextAction::SelectSupportedVersion),
            Self::UnknownCriticalExtension { namespace } => PublicError::builder(
                ErrorCode::UnsupportedCapability,
                "a critical configuration extension is unsupported",
            )
            .detail(
                detail_key("extension"),
                rootlight_error::PublicValue::Label(safe_label_or_fallback(namespace)),
            ),
            _ => PublicError::builder(ErrorCode::InvalidArgument, "configuration is invalid")
                .next_action(NextAction::CorrectField {
                    field: detail_key("configuration"),
                }),
        };
        builder.build().unwrap_or_else(|_| {
            PublicError::builder(ErrorCode::Internal, "internal operation failed")
                .build()
                .unwrap_or_else(|_| unreachable!("closed fallback error is statically bounded"))
        })
    }
}

impl<'de> Deserialize<'de> for ContractVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl Serialize for ContractVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_security_fields_without_echoing_values() {
        let error = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: r#"
version = "1.0"
[security]
network = "deny"
credential = "gho_fake_secret"
"#,
        }])
        .expect_err("unknown security field must be rejected");

        let public = serde_json::to_string(&error.to_public()).expect("public error serializes");
        assert!(!public.contains("gho_fake_secret"));
        assert!(!public.contains("credential"));
    }

    #[test]
    fn hard_denials_cannot_be_weakened() {
        let snapshot = ConfigSnapshot::resolve(&[
            ConfigLayer {
                source: ConfigSource::System,
                contents: r#"
version = "1.0"
[security]
network = "deny"
repository_execution = "deny"
in_process_native_plugins = "deny"
"#,
            },
            ConfigLayer {
                source: ConfigSource::Repository,
                contents: r#"
version = "1.0"
[security]
network = "loopback"
repository_execution = "explicit_consent"
in_process_native_plugins = "deny"
"#,
            },
        ])
        .expect("valid layers resolve");

        assert_eq!(snapshot.security().network, NetworkPolicy::Deny);
        assert_eq!(
            snapshot.security().repository_execution,
            RepositoryExecutionPolicy::Deny
        );
    }

    #[test]
    fn canonical_snapshot_is_independent_of_layer_input_order() {
        let system = ConfigLayer {
            source: ConfigSource::System,
            contents: r#"
version = "1.0"
[resources]
max_results = 75
"#,
        };
        let user = ConfigLayer {
            source: ConfigSource::User,
            contents: r#"
version = "1.0"
[analysis]
default_tier = "deep"
"#,
        };
        let first = ConfigSnapshot::resolve(&[system, user]).expect("valid layers resolve");
        let second = ConfigSnapshot::resolve(&[user, system]).expect("valid layers resolve");

        assert_eq!(first.canonical_bytes(), second.canonical_bytes());
        assert_eq!(first.hash(), second.hash());
    }

    #[test]
    fn optional_extensions_are_preserved_without_payload_debug() {
        let snapshot = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: r#"
version = "1.0"
[extensions.example]
version = "1.0"
critical = false
[extensions.example.data]
token = "gho_fake_secret"
mode = "fixture"
"#,
        }])
        .expect("optional extension is preserved");
        let debug = format!("{:?}", snapshot.extensions().get("example"));

        assert!(debug.contains("ExtensionSnapshot"));
        assert!(!debug.contains("gho_fake_secret"));
        assert!(!debug.contains("fixture"));
    }

    #[test]
    fn unknown_critical_extension_is_rejected() {
        let result = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: r#"
version = "1.0"
[extensions.future]
version = "1.0"
critical = true
"#,
        }]);
        assert!(matches!(
            result,
            Err(ConfigError::UnknownCriticalExtension { namespace }) if namespace == "future"
        ));
    }

    #[test]
    fn unsupported_major_maps_to_protocol_mismatch() {
        let error = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: "version = \"2.0\"",
        }])
        .expect_err("major two is unsupported");
        assert_eq!(error.to_public().code(), ErrorCode::ProtocolMismatch);
    }

    #[test]
    fn accepts_additive_minor_versions_and_omitted_optional_sections() {
        let snapshot = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: "version = \"1.2\"",
        }])
        .expect("minor versions are additive");

        assert_eq!(snapshot.version(), CONFIG_VERSION);
        assert_eq!(snapshot.security(), SecurityConfig::default());
        assert_eq!(snapshot.resources(), ResourceConfig::default());
    }
}
