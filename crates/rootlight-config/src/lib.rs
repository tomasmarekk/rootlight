//! Versioned, immutable configuration contracts for Rootlight.
//!
//! Callers supply bytes for explicit configuration layers. This crate never
//! reads ambient files, environment variables, credentials, or network state.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use rootlight_error::{DetailKey, ErrorCode, NextAction, PublicError, SafeLabel};
use rootlight_ids::{ContentHash, content_hash};
use serde::{Deserialize, Serialize};

/// The frozen initial production configuration contract version.
pub const CONFIG_VERSION_1_0: ContractVersion = ContractVersion::new(1, 0);
/// The current production configuration contract version.
pub const CONFIG_VERSION: ContractVersion = ContractVersion::new(1, 1);
/// Default source bytes available to one source-bearing response.
pub const DEFAULT_MAX_SOURCE_RESPONSE_BYTES: u64 = 64 * 1024;
/// Hard source-byte ceiling for one source-bearing response in configuration 1.1.
pub const MAX_SOURCE_RESPONSE_BYTES: u64 = 512 * 1024;
/// Default bytes accepted from one source file for discovery and analysis.
pub const DEFAULT_MAX_SOURCE_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Hard bytes accepted from one source file for discovery and analysis.
pub const MAX_SOURCE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const CONFIG_V1_0_MAX_SOURCE_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum number of explicit configuration layers in one resolution.
pub const MAX_CONFIG_LAYERS: usize = 16;
/// Maximum UTF-8 bytes accepted for one configuration layer.
pub const MAX_CONFIG_LAYER_BYTES: usize = 256 * 1024;
/// Maximum aggregate UTF-8 bytes accepted across all layers.
pub const MAX_CONFIG_TOTAL_BYTES: usize = 1024 * 1024;
/// Maximum nesting depth accepted in one extension payload.
pub const MAX_EXTENSION_DEPTH: usize = 32;
/// Maximum scalar, array, table, and member nodes in one extension payload.
pub const MAX_EXTENSION_NODES: usize = 8_192;
/// Maximum canonical JSON bytes retained for one extension payload.
pub const MAX_EXTENSION_BYTES: usize = 64 * 1024;

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
            "pattern": "^1\\.(0|[1-9][0-9]{0,3}|[1-5][0-9]{4}|6[0-4][0-9]{3}|65[0-4][0-9]{2}|655[0-2][0-9]|6553[0-5])$",
        })
    }
}

/// Configuration layer, ordered from baseline to highest normal value precedence.
///
/// Explicit denials use separate sticky authority and cannot be weakened by a
/// later layer even though ordinary values follow this ordering.
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

/// Bounded response resource configuration for contract-level operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ResourceConfig {
    /// Maximum source bytes one source-bearing response may return.
    ///
    /// Configuration 1.0 snapshots can retain their frozen 16 MiB legacy
    /// ceiling. Configuration 1.1 snapshots are bounded by
    /// [`MAX_SOURCE_RESPONSE_BYTES`].
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 524_288)))]
    pub max_source_bytes: u64,
    /// Maximum result records one contract operation may return.
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 10_000)))]
    pub max_results: u32,
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            max_source_bytes: DEFAULT_MAX_SOURCE_RESPONSE_BYTES,
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
    /// Maximum bytes accepted from one source file for discovery and analysis.
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 67_108_864)))]
    pub max_source_file_bytes: u64,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            default_tier: AnalysisTier::Structural,
            max_source_file_bytes: DEFAULT_MAX_SOURCE_FILE_BYTES,
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
    hard_denial_provenance: BTreeMap<String, ConfigSource>,
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
        validate_layer_bounds(layers)?;
        let mut state = EffectiveConfig::defaults();
        let mut ordered = layers.to_vec();
        ordered.sort_by_key(|layer| layer.source);
        for layer in ordered {
            let parsed = decode_wire_config(layer.contents)?;
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

    /// Returns the source that supplied each effective configuration value.
    #[must_use]
    pub const fn provenance(&self) -> &BTreeMap<String, ConfigSource> {
        &self.provenance
    }

    /// Returns explicit sticky denials and their highest-authority sources.
    ///
    /// Safe fallback denials from compiled defaults are intentionally omitted.
    #[must_use]
    pub const fn hard_denial_provenance(&self) -> &BTreeMap<String, ConfigSource> {
        &self.hard_denial_provenance
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
    hard_denial_provenance: BTreeMap<String, ConfigSource>,
    saw_explicit_layer: bool,
    uses_current_contract: bool,
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
            "analysis.max_source_file_bytes",
        ] {
            provenance.insert(field.to_owned(), ConfigSource::Defaults);
        }
        Self {
            security: SecurityConfig::default(),
            resources: ResourceConfig::default(),
            analysis: AnalysisConfig::default(),
            extensions: BTreeMap::new(),
            provenance,
            hard_denial_provenance: BTreeMap::new(),
            saw_explicit_layer: false,
            uses_current_contract: false,
        }
    }

    fn apply(&mut self, source: ConfigSource, wire: DecodedConfig) -> Result<(), ConfigError> {
        self.saw_explicit_layer = true;
        self.uses_current_contract |= wire.uses_current_contract;
        if let Some(security) = wire.security {
            if let Some(network) = security.network {
                self.apply_network_policy(source, network)?;
            }
            if let Some(execution) = security.repository_execution {
                self.apply_execution_policy(source, execution)?;
            }
            if security.in_process_native_plugins.is_some() {
                self.security.in_process_native_plugins = NativePluginPolicy::Deny;
                self.provenance
                    .insert("security.in_process_native_plugins".to_owned(), source);
            }
        }
        if let Some(resources) = wire.resources {
            if let Some(max_source_bytes) = resources.max_source_bytes {
                let maximum = if wire.uses_current_contract {
                    MAX_SOURCE_RESPONSE_BYTES
                } else {
                    CONFIG_V1_0_MAX_SOURCE_RESPONSE_BYTES
                };
                if !(1..=maximum).contains(&max_source_bytes) {
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
        if let Some(analysis) = wire.analysis {
            if let Some(default_tier) = analysis.default_tier {
                self.analysis.default_tier = default_tier;
                self.provenance
                    .insert("analysis.default_tier".to_owned(), source);
            }
            if let Some(max_source_file_bytes) = analysis.max_source_file_bytes {
                if !(1..=MAX_SOURCE_FILE_BYTES).contains(&max_source_file_bytes) {
                    return Err(ConfigError::ResourceLimitOutOfRange {
                        field: "max_source_file_bytes",
                    });
                }
                self.analysis.max_source_file_bytes = max_source_file_bytes;
                self.provenance
                    .insert("analysis.max_source_file_bytes".to_owned(), source);
            }
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

    fn apply_network_policy(
        &mut self,
        source: ConfigSource,
        requested: NetworkPolicy,
    ) -> Result<(), ConfigError> {
        const FIELD: &str = "security.network";
        if source == ConfigSource::Repository && requested == NetworkPolicy::Loopback {
            return Err(ConfigError::RepositorySecurityEscalation { field: FIELD });
        }
        if self.hard_denial_provenance.contains_key(FIELD) {
            return Ok(());
        }
        self.security.network = requested;
        self.provenance.insert(FIELD.to_owned(), source);
        if requested == NetworkPolicy::Deny && source != ConfigSource::Defaults {
            self.hard_denial_provenance
                .entry(FIELD.to_owned())
                .or_insert(source);
        }
        Ok(())
    }

    fn apply_execution_policy(
        &mut self,
        source: ConfigSource,
        requested: RepositoryExecutionPolicy,
    ) -> Result<(), ConfigError> {
        const FIELD: &str = "security.repository_execution";
        if source == ConfigSource::Repository
            && requested == RepositoryExecutionPolicy::ExplicitConsent
        {
            return Err(ConfigError::RepositorySecurityEscalation { field: FIELD });
        }
        if self.hard_denial_provenance.contains_key(FIELD) {
            return Ok(());
        }
        self.security.repository_execution = requested;
        self.provenance.insert(FIELD.to_owned(), source);
        if requested == RepositoryExecutionPolicy::Deny && source != ConfigSource::Defaults {
            self.hard_denial_provenance
                .entry(FIELD.to_owned())
                .or_insert(source);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<ConfigSnapshot, ConfigError> {
        let version = if !self.saw_explicit_layer || self.uses_current_contract {
            CONFIG_VERSION
        } else {
            CONFIG_VERSION_1_0
        };
        if version == CONFIG_VERSION && self.resources.max_source_bytes > MAX_SOURCE_RESPONSE_BYTES
        {
            return Err(ConfigError::ResourceLimitOutOfRange {
                field: "max_source_bytes",
            });
        }
        let canonical = if version == CONFIG_VERSION_1_0 {
            self.provenance.remove("analysis.max_source_file_bytes");
            serde_json::to_vec(&CanonicalConfigV1_0 {
                version,
                security: self.security,
                resources: CanonicalResourceConfigV1_0 {
                    max_source_bytes: self.resources.max_source_bytes,
                    max_results: self.resources.max_results,
                },
                analysis: CanonicalAnalysisConfigV1_0 {
                    default_tier: self.analysis.default_tier,
                },
                extensions: self.extensions.clone(),
            })
        } else {
            serde_json::to_vec(&CanonicalConfig {
                version,
                security: self.security,
                resources: self.resources,
                analysis: self.analysis,
                extensions: self.extensions.clone(),
            })
        }
        .map_err(|source| ConfigError::Canonicalize { source })?;
        let hash = content_hash(&canonical);
        Ok(ConfigSnapshot {
            version,
            security: self.security,
            resources: self.resources,
            analysis: self.analysis,
            extensions: self.extensions,
            provenance: self.provenance,
            hard_denial_provenance: self.hard_denial_provenance,
            canonical,
            hash,
        })
    }
}

fn validate_layer_bounds(layers: &[ConfigLayer<'_>]) -> Result<(), ConfigError> {
    if layers.len() > MAX_CONFIG_LAYERS {
        return Err(ConfigError::TooManyLayers {
            maximum: MAX_CONFIG_LAYERS,
        });
    }
    let mut total = 0_usize;
    for layer in layers {
        let bytes = layer.contents.len();
        if bytes > MAX_CONFIG_LAYER_BYTES {
            return Err(ConfigError::LayerTooLarge {
                maximum: MAX_CONFIG_LAYER_BYTES,
            });
        }
        total = total
            .checked_add(bytes)
            .ok_or(ConfigError::TotalLayersTooLarge {
                maximum: MAX_CONFIG_TOTAL_BYTES,
            })?;
        if total > MAX_CONFIG_TOTAL_BYTES {
            return Err(ConfigError::TotalLayersTooLarge {
                maximum: MAX_CONFIG_TOTAL_BYTES,
            });
        }
    }
    Ok(())
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
    let mut encoder = ExtensionEncoder::default();
    encoder.write_value(data, 0)?;
    Ok(encoder.output)
}

#[derive(Debug, Default)]
struct ExtensionEncoder {
    output: Vec<u8>,
    nodes: usize,
}

impl ExtensionEncoder {
    fn write_value(&mut self, value: &toml::Value, depth: usize) -> Result<(), ConfigError> {
        if depth > MAX_EXTENSION_DEPTH {
            return Err(ConfigError::ExtensionTooDeep);
        }
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or(ConfigError::ExtensionTooManyNodes)?;
        if self.nodes > MAX_EXTENSION_NODES {
            return Err(ConfigError::ExtensionTooManyNodes);
        }
        match value {
            toml::Value::String(value) => self.write_json(value),
            toml::Value::Integer(value) => self.write_bytes(value.to_string().as_bytes()),
            toml::Value::Boolean(value) => {
                self.write_bytes(if *value { b"true" } else { b"false" })
            }
            toml::Value::Array(values) => {
                self.write_byte(b'[')?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        self.write_byte(b',')?;
                    }
                    self.write_value(value, depth + 1)?;
                }
                self.write_byte(b']')
            }
            toml::Value::Table(values) => {
                self.write_byte(b'{')?;
                for (index, (key, value)) in values.iter().enumerate() {
                    if index > 0 {
                        self.write_byte(b',')?;
                    }
                    self.write_json(key)?;
                    self.write_byte(b':')?;
                    self.write_value(value, depth + 1)?;
                }
                self.write_byte(b'}')
            }
            toml::Value::Float(_) | toml::Value::Datetime(_) => {
                Err(ConfigError::UnsupportedExtensionValue)
            }
        }
    }

    fn write_json(&mut self, value: &str) -> Result<(), ConfigError> {
        let encoded =
            serde_json::to_vec(value).map_err(|source| ConfigError::Canonicalize { source })?;
        self.write_bytes(&encoded)
    }

    fn write_byte(&mut self, byte: u8) -> Result<(), ConfigError> {
        if self.output.len() == MAX_EXTENSION_BYTES {
            return Err(ConfigError::ExtensionTooLarge);
        }
        self.output.push(byte);
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), ConfigError> {
        let new_length = self
            .output
            .len()
            .checked_add(bytes.len())
            .ok_or(ConfigError::ExtensionTooLarge)?;
        if new_length > MAX_EXTENSION_BYTES {
            return Err(ConfigError::ExtensionTooLarge);
        }
        self.output.extend_from_slice(bytes);
        Ok(())
    }
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

/// JSON Schema marker for the frozen strict configuration 1.0 document.
///
/// Layer source and semantic validation are applied only by
/// [`ConfigSnapshot::resolve`]; this marker intentionally has no Serde decoder.
#[derive(Debug)]
pub struct ConfigDocumentSchema;

#[cfg(feature = "schema")]
impl schemars::JsonSchema for ConfigDocumentSchema {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ConfigDocumentSchema".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = generator.subschema_for::<WireConfig>();
        schema.insert("title".to_owned(), "Rootlight configuration 1.0".into());
        schema
    }
}

/// JSON Schema marker for the strict configuration 1.1 document.
///
/// This schema separates bounded source-bearing responses from source-file
/// bytes accepted by discovery and analysis.
#[derive(Debug)]
pub struct ConfigDocumentSchemaV1_1;

#[cfg(feature = "schema")]
impl schemars::JsonSchema for ConfigDocumentSchemaV1_1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ConfigDocumentSchemaV1_1".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = generator.subschema_for::<WireConfigV1_1>();
        schema.insert("title".to_owned(), "Rootlight configuration 1.1".into());
        schema
    }
}

fn decode_wire_config(contents: &str) -> Result<DecodedConfig, ConfigError> {
    let probe: WireVersionProbe =
        toml::from_str(contents).map_err(|source| ConfigError::Parse { source })?;
    probe.version.require_supported()?;
    if probe.version.minor() == 0 {
        toml::from_str::<WireConfig>(contents)
            .map(DecodedConfig::from)
            .map_err(|source| ConfigError::Parse { source })
    } else {
        toml::from_str::<WireConfigV1_1>(contents)
            .map(DecodedConfig::from)
            .map_err(|source| ConfigError::Parse { source })
    }
}

#[derive(Debug, Deserialize)]
struct WireVersionProbe {
    version: ContractVersion,
}

#[cfg(feature = "schema")]
struct ConfigVersionV1_1Schema;

#[cfg(feature = "schema")]
impl schemars::JsonSchema for ConfigVersionV1_1Schema {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ConfigVersionV1_1".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^1\\.([1-9][0-9]{0,3}|[1-5][0-9]{4}|6[0-4][0-9]{3}|65[0-4][0-9]{2}|655[0-2][0-9]|6553[0-5])$",
        })
    }
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct WireConfig {
    version: ContractVersion,
    #[serde(default)]
    security: Option<PartialSecurityConfig>,
    #[serde(default)]
    resources: Option<PartialResourceConfig>,
    #[serde(default)]
    analysis: Option<PartialAnalysisConfig>,
    #[serde(default)]
    #[cfg_attr(feature = "schema", schemars(with = "ExtensionMapSchema"))]
    extensions: BTreeMap<String, WireExtension>,
}

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

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct WireConfigV1_1 {
    #[cfg_attr(feature = "schema", schemars(with = "ConfigVersionV1_1Schema"))]
    version: ContractVersion,
    #[serde(default)]
    security: Option<PartialSecurityConfig>,
    #[serde(default)]
    resources: Option<PartialResourceConfigV1_1>,
    #[serde(default)]
    analysis: Option<PartialAnalysisConfigV1_1>,
    #[serde(default)]
    #[cfg_attr(feature = "schema", schemars(with = "ExtensionMapSchema"))]
    extensions: BTreeMap<String, WireExtension>,
}

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct PartialResourceConfigV1_1 {
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 524_288)))]
    max_source_bytes: Option<u64>,
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 10_000)))]
    max_results: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct PartialAnalysisConfigV1_1 {
    default_tier: Option<AnalysisTier>,
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 67_108_864)))]
    max_source_file_bytes: Option<u64>,
}

#[derive(Debug)]
struct DecodedConfig {
    uses_current_contract: bool,
    security: Option<PartialSecurityConfig>,
    resources: Option<DecodedResourceConfig>,
    analysis: Option<DecodedAnalysisConfig>,
    extensions: BTreeMap<String, WireExtension>,
}

#[derive(Debug)]
struct DecodedResourceConfig {
    max_source_bytes: Option<u64>,
    max_results: Option<u32>,
}

#[derive(Debug)]
struct DecodedAnalysisConfig {
    default_tier: Option<AnalysisTier>,
    max_source_file_bytes: Option<u64>,
}

impl From<WireConfig> for DecodedConfig {
    fn from(wire: WireConfig) -> Self {
        let WireConfig {
            version,
            security,
            resources,
            analysis,
            extensions,
        } = wire;
        Self {
            uses_current_contract: version.minor() != 0,
            security,
            resources: resources.map(|resources| DecodedResourceConfig {
                max_source_bytes: resources.max_source_bytes,
                max_results: resources.max_results,
            }),
            analysis: analysis.map(|analysis| DecodedAnalysisConfig {
                default_tier: analysis.default_tier,
                max_source_file_bytes: None,
            }),
            extensions,
        }
    }
}

impl From<WireConfigV1_1> for DecodedConfig {
    fn from(wire: WireConfigV1_1) -> Self {
        let WireConfigV1_1 {
            version,
            security,
            resources,
            analysis,
            extensions,
        } = wire;
        Self {
            uses_current_contract: version.minor() != 0,
            security,
            resources: resources.map(|resources| DecodedResourceConfig {
                max_source_bytes: resources.max_source_bytes,
                max_results: resources.max_results,
            }),
            analysis: analysis.map(|analysis| DecodedAnalysisConfig {
                default_tier: analysis.default_tier,
                max_source_file_bytes: analysis.max_source_file_bytes,
            }),
            extensions,
        }
    }
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct WireExtension {
    version: ContractVersion,
    critical: bool,
    #[serde(default = "empty_extension_data")]
    #[cfg_attr(feature = "schema", schemars(with = "ExtensionDataSchema"))]
    data: toml::Value,
}

#[cfg(feature = "schema")]
#[allow(dead_code, reason = "variants define the generated recursive schema")]
#[derive(schemars::JsonSchema)]
#[serde(untagged)]
enum ExtensionDataSchema {
    String(String),
    Integer(i64),
    Boolean(bool),
    Array(Vec<ExtensionDataSchema>),
    Object(BTreeMap<String, ExtensionDataSchema>),
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

#[derive(Debug, Serialize)]
struct CanonicalConfigV1_0 {
    version: ContractVersion,
    security: SecurityConfig,
    resources: CanonicalResourceConfigV1_0,
    analysis: CanonicalAnalysisConfigV1_0,
    extensions: BTreeMap<String, ExtensionSnapshot>,
}

#[derive(Debug, Serialize)]
struct CanonicalResourceConfigV1_0 {
    max_source_bytes: u64,
    max_results: u32,
}

#[derive(Debug, Serialize)]
struct CanonicalAnalysisConfigV1_0 {
    default_tier: AnalysisTier,
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
    /// Too many explicit layers were supplied.
    #[error("configuration has too many layers; maximum is {maximum}")]
    TooManyLayers {
        /// Maximum accepted layer count.
        maximum: usize,
    },
    /// One layer exceeded the pre-parse byte limit.
    #[error("configuration layer exceeds its byte limit; maximum is {maximum}")]
    LayerTooLarge {
        /// Maximum accepted UTF-8 bytes per layer.
        maximum: usize,
    },
    /// The aggregate layer bytes exceeded the pre-parse limit.
    #[error("configuration layers exceed their aggregate byte limit; maximum is {maximum}")]
    TotalLayersTooLarge {
        /// Maximum accepted UTF-8 bytes across all layers.
        maximum: usize,
    },
    /// Repository-controlled configuration attempted to enable a protected capability.
    #[error("repository configuration cannot widen {field}")]
    RepositorySecurityEscalation {
        /// Stable source-free field name.
        field: &'static str,
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
    /// Extension data exceeded its nesting-depth limit.
    #[error("configuration extension exceeds its nesting-depth limit")]
    ExtensionTooDeep,
    /// Extension data exceeded its node-count limit.
    #[error("configuration extension exceeds its node-count limit")]
    ExtensionTooManyNodes,
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
    fn trusted_permissions_override_fallback_denials() {
        let snapshot = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::System,
            contents: r#"
version = "1.0"
[security]
network = "loopback"
repository_execution = "explicit_consent"
"#,
        }])
        .expect("trusted permissions resolve");

        assert_eq!(snapshot.security().network, NetworkPolicy::Loopback);
        assert_eq!(
            snapshot.security().repository_execution,
            RepositoryExecutionPolicy::ExplicitConsent
        );
        assert_eq!(
            snapshot.provenance().get("security.network"),
            Some(&ConfigSource::System)
        );
        assert!(snapshot.hard_denial_provenance().is_empty());
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
"#,
            },
            ConfigLayer {
                source: ConfigSource::User,
                contents: r#"
version = "1.0"
[security]
network = "loopback"
repository_execution = "explicit_consent"
"#,
            },
        ])
        .expect("valid layers resolve");

        assert_eq!(snapshot.security().network, NetworkPolicy::Deny);
        assert_eq!(
            snapshot.security().repository_execution,
            RepositoryExecutionPolicy::Deny
        );
        assert_eq!(
            snapshot.provenance().get("security.network"),
            Some(&ConfigSource::System)
        );
        assert_eq!(
            snapshot
                .hard_denial_provenance()
                .get("security.repository_execution"),
            Some(&ConfigSource::System)
        );
    }

    #[test]
    fn repository_cannot_enable_protected_capabilities() {
        for (field, contents) in [
            (
                "security.network",
                "version = \"1.0\"\n[security]\nnetwork = \"loopback\"\n",
            ),
            (
                "security.repository_execution",
                "version = \"1.0\"\n[security]\nrepository_execution = \"explicit_consent\"\n",
            ),
        ] {
            assert!(matches!(
                ConfigSnapshot::resolve(&[ConfigLayer {
                    source: ConfigSource::Repository,
                    contents,
                }]),
                Err(ConfigError::RepositorySecurityEscalation { field: observed })
                    if observed == field
            ));
        }
    }

    #[test]
    fn repository_denial_blocks_operation_permission() {
        let snapshot = ConfigSnapshot::resolve(&[
            ConfigLayer {
                source: ConfigSource::Repository,
                contents: "version = \"1.0\"\n[security]\nnetwork = \"deny\"\n",
            },
            ConfigLayer {
                source: ConfigSource::Operation,
                contents: "version = \"1.0\"\n[security]\nnetwork = \"loopback\"\n",
            },
        ])
        .expect("repository denial remains effective");

        assert_eq!(snapshot.security().network, NetworkPolicy::Deny);
        assert_eq!(
            snapshot.hard_denial_provenance().get("security.network"),
            Some(&ConfigSource::Repository)
        );
    }

    #[test]
    fn provenance_does_not_change_canonical_identity() {
        let defaults = ConfigSnapshot::resolve(&[]).expect("defaults resolve");
        let explicit = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::System,
            contents: r#"
version = "1.1"
[security]
network = "deny"
repository_execution = "deny"
"#,
        }])
        .expect("explicit denials resolve");

        assert_eq!(defaults.canonical_bytes(), explicit.canonical_bytes());
        assert_eq!(defaults.hash(), explicit.hash());
        assert!(defaults.hard_denial_provenance().is_empty());
        assert!(!explicit.hard_denial_provenance().is_empty());
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
    fn frozen_1_0_contract_keeps_legacy_response_limit_and_canonical_shape() {
        let snapshot = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: r#"
version = "1.0"
[resources]
max_source_bytes = 16777216
"#,
        }])
        .expect("frozen configuration resolves");

        assert_eq!(snapshot.version(), CONFIG_VERSION_1_0);
        assert_eq!(
            snapshot.resources().max_source_bytes,
            CONFIG_V1_0_MAX_SOURCE_RESPONSE_BYTES
        );
        assert_eq!(
            snapshot.analysis().max_source_file_bytes,
            DEFAULT_MAX_SOURCE_FILE_BYTES
        );
        assert!(
            !snapshot
                .provenance()
                .contains_key("analysis.max_source_file_bytes")
        );
        assert_eq!(
            std::str::from_utf8(snapshot.canonical_bytes())
                .expect("canonical configuration is UTF-8"),
            r#"{"version":"1.0","security":{"network":"deny","repository_execution":"deny","in_process_native_plugins":"deny"},"resources":{"max_source_bytes":16777216,"max_results":50},"analysis":{"default_tier":"structural"},"extensions":{}}"#
        );

        assert!(matches!(
            ConfigSnapshot::resolve(&[ConfigLayer {
                source: ConfigSource::User,
                contents: "version = \"1.0\"\n[analysis]\nmax_source_file_bytes = 8388608\n",
            }]),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn version_1_1_separates_response_and_analysis_source_limits() {
        let snapshot = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: r#"
version = "1.1"
[resources]
max_source_bytes = 524288
[analysis]
max_source_file_bytes = 67108864
"#,
        }])
        .expect("configuration 1.1 resolves");

        assert_eq!(snapshot.version(), CONFIG_VERSION);
        assert_eq!(
            snapshot.resources().max_source_bytes,
            MAX_SOURCE_RESPONSE_BYTES
        );
        assert_eq!(
            snapshot.analysis().max_source_file_bytes,
            MAX_SOURCE_FILE_BYTES
        );
        assert_eq!(
            snapshot.provenance().get("analysis.max_source_file_bytes"),
            Some(&ConfigSource::User)
        );

        for (field, contents) in [
            (
                "max_source_bytes",
                "version = \"1.1\"\n[resources]\nmax_source_bytes = 524289\n",
            ),
            (
                "max_source_file_bytes",
                "version = \"1.1\"\n[analysis]\nmax_source_file_bytes = 67108865\n",
            ),
        ] {
            assert!(matches!(
                ConfigSnapshot::resolve(&[ConfigLayer {
                    source: ConfigSource::User,
                    contents,
                }]),
                Err(ConfigError::ResourceLimitOutOfRange { field: observed })
                    if observed == field
            ));
        }
    }

    #[test]
    fn current_layer_cannot_inherit_wide_legacy_response_limit() {
        let result = ConfigSnapshot::resolve(&[
            ConfigLayer {
                source: ConfigSource::System,
                contents: "version = \"1.0\"\n[resources]\nmax_source_bytes = 16777216\n",
            },
            ConfigLayer {
                source: ConfigSource::User,
                contents: "version = \"1.1\"\n[analysis]\ndefault_tier = \"deep\"\n",
            },
        ]);

        assert!(matches!(
            result,
            Err(ConfigError::ResourceLimitOutOfRange {
                field: "max_source_bytes"
            })
        ));
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
    fn configuration_input_bounds_fail_before_parsing() {
        let too_many = vec![
            ConfigLayer {
                source: ConfigSource::User,
                contents: "version = \"1.0\"",
            };
            MAX_CONFIG_LAYERS + 1
        ];
        assert!(matches!(
            ConfigSnapshot::resolve(&too_many),
            Err(ConfigError::TooManyLayers { .. })
        ));

        let oversized = "x".repeat(MAX_CONFIG_LAYER_BYTES + 1);
        assert!(matches!(
            ConfigSnapshot::resolve(&[ConfigLayer {
                source: ConfigSource::User,
                contents: &oversized,
            }]),
            Err(ConfigError::LayerTooLarge { .. })
        ));
    }

    #[test]
    fn extension_canonicalization_enforces_type_and_byte_bounds() {
        let exact_value = "x".repeat(MAX_EXTENSION_BYTES - 2);
        let exact = format!(
            "version = \"1.0\"\n[extensions.example]\nversion = \"1.0\"\ncritical = false\ndata = {exact_value:?}\n"
        );
        let snapshot = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: &exact,
        }])
        .expect("exact canonical extension limit is accepted");
        assert_eq!(
            snapshot.extensions()["example"].bytes(),
            MAX_EXTENSION_BYTES as u64
        );

        let oversized_value = "x".repeat(MAX_EXTENSION_BYTES - 1);
        let oversized = format!(
            "version = \"1.0\"\n[extensions.example]\nversion = \"1.0\"\ncritical = false\ndata = {oversized_value:?}\n"
        );
        assert!(matches!(
            ConfigSnapshot::resolve(&[ConfigLayer {
                source: ConfigSource::User,
                contents: &oversized,
            }]),
            Err(ConfigError::ExtensionTooLarge)
        ));

        assert!(matches!(
            ConfigSnapshot::resolve(&[ConfigLayer {
                source: ConfigSource::User,
                contents: "version = \"1.0\"\n[extensions.example]\nversion = \"1.0\"\ncritical = false\ndata = 1.5\n",
            }]),
            Err(ConfigError::UnsupportedExtensionValue)
        ));
    }

    #[test]
    fn extension_canonicalization_enforces_depth_and_node_bounds() {
        let mut nested = toml::Value::String("leaf".to_owned());
        for _ in 0..=MAX_EXTENSION_DEPTH {
            nested = toml::Value::Array(vec![nested]);
        }
        assert!(matches!(
            canonical_extension(&nested),
            Err(ConfigError::ExtensionTooDeep)
        ));

        let many = toml::Value::Array(
            (0..MAX_EXTENSION_NODES)
                .map(|_| toml::Value::Boolean(true))
                .collect(),
        );
        assert!(matches!(
            canonical_extension(&many),
            Err(ConfigError::ExtensionTooManyNodes)
        ));
    }

    #[test]
    fn contract_version_enforces_u16_minor_boundary() {
        let parsed = ContractVersion::parse("1.65535").expect("maximum minor is valid");
        assert_eq!(parsed, ContractVersion::new(1, 65_535));
        for invalid in ["1.65536", "1.01", "1", "1.0.0"] {
            assert!(matches!(
                ContractVersion::parse(invalid),
                Err(ConfigError::InvalidVersion)
            ));
        }
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
        assert_eq!(snapshot.analysis(), AnalysisConfig::default());
    }
}
