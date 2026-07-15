//! Generates and verifies checked protobuf and JSON Schema artifacts.
//!
//! Generation happens in a temporary tree before update or comparison so a
//! failed compiler run cannot leave a partially updated public contract.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use cargo_metadata::MetadataCommand;
use prost::Message;
use rootlight_config::ConfigDocumentSchema;
use rootlight_ir::IrDocumentSchema;
use rootlight_mcp_contract::{ErrorResponse, ResponseMetadata};
use rootlight_protocol::generated::common::v1::ContractVersion as ProtocolContractVersion;
use schemars::{JsonSchema, generate::SchemaSettings};
use serde::{Deserialize, Serialize};

const MANIFEST_VERSION: &str = "1.0";
const PROTO_FILES: [&str; 3] = ["common.proto", "daemon_v1.proto", "adapter_v1.proto"];
const GENERATED_RUST_FILES: [&str; 3] = [
    "rootlight.common.v1.rs",
    "rootlight.daemon.v1.rs",
    "rootlight.adapter.v1.rs",
];
const SCHEMA_ROOT: &str = "schemas/generated";
const PROTOCOL_GENERATED_ROOT: &str = "crates/rootlight-protocol/src/generated";
const COMPATIBILITY_ROOT: &str = "tests/fixtures/compatibility";
const COMPATIBILITY_FILES: [&str; 4] = [
    "contract-0.1.json",
    "contract-0.2.json",
    "contract-1.0.json",
    "contract-2.0-rejected.json",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GenerateMode {
    Update,
    Check,
}

pub(crate) fn generate(mode: GenerateMode) -> Result<(), SchemaError> {
    let workspace_root = workspace_root()?;
    let temporary = tempfile::tempdir().map_err(SchemaError::CreateTemporaryDirectory)?;
    let staged_root = temporary.path();

    generate_protobuf(&workspace_root, staged_root)?;
    generate_json_schemas(staged_root)?;
    validate_generated_json_schemas(staged_root)?;
    generate_manifest(&workspace_root, staged_root)?;

    match mode {
        GenerateMode::Update => update_outputs(&workspace_root, staged_root)?,
        GenerateMode::Check => check_outputs(&workspace_root, staged_root)?,
    }

    let action = match mode {
        GenerateMode::Update => "updated",
        GenerateMode::Check => "verified",
    };
    println!("schema artifacts {action}");
    Ok(())
}

pub(crate) fn check_compatibility() -> Result<(), SchemaError> {
    let workspace_root = workspace_root()?;
    let fixtures = load_compatibility_fixtures(&workspace_root)?;
    if fixtures
        .iter()
        .any(|fixture| fixture.fixture_version != MANIFEST_VERSION)
    {
        return Err(SchemaError::UnsupportedCompatibilityFixtureVersion);
    }
    let observed_versions: Vec<String> = fixtures
        .iter()
        .map(|fixture| fixture.contract_version.clone())
        .collect();
    if observed_versions != ["0.1", "0.2", "1.0", "2.0"] {
        return Err(SchemaError::CompatibilityFixtureSet(observed_versions));
    }

    for fixture in fixtures {
        match fixture.disposition {
            FixtureDisposition::DraftLegacy | FixtureDisposition::Production => {
                if fixture.disposition == FixtureDisposition::Production
                    && fixture.contract_version != MANIFEST_VERSION
                {
                    return Err(SchemaError::CompatibilityDispositionMismatch {
                        version: fixture.contract_version,
                    });
                }
                let configuration = fixture.configuration.ok_or_else(|| {
                    SchemaError::CompatibilityMissingConfiguration {
                        version: fixture.contract_version.clone(),
                    }
                })?;
                let version = configuration
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| SchemaError::CompatibilityMissingConfiguration {
                        version: fixture.contract_version.clone(),
                    })?;
                let parsed = rootlight_config::ContractVersion::parse(version).map_err(|_| {
                    SchemaError::CompatibilityConfigurationRejected {
                        version: fixture.contract_version.clone(),
                    }
                })?;
                parsed.require_supported().map_err(|_| {
                    SchemaError::CompatibilityConfigurationRejected {
                        version: fixture.contract_version.clone(),
                    }
                })?;
                serde_json::from_value::<ConfigDocumentSchema>(configuration.clone()).map_err(
                    |source| SchemaError::CompatibilityConfigurationDecode {
                        version: fixture.contract_version.clone(),
                        source,
                    },
                )?;
                validate_configuration_semantics(&fixture.contract_version, &configuration)?;
                let metadata = fixture.mcp_response_metadata.ok_or_else(|| {
                    SchemaError::CompatibilityMissingMcpMetadata {
                        version: fixture.contract_version.clone(),
                    }
                })?;
                serde_json::from_value::<ResponseMetadata>(metadata).map_err(|source| {
                    SchemaError::CompatibilityMcpRejected {
                        version: fixture.contract_version,
                        source,
                    }
                })?;
            }
            FixtureDisposition::UnsupportedMajor => {
                let configuration = fixture.configuration.ok_or_else(|| {
                    SchemaError::CompatibilityMissingConfiguration {
                        version: fixture.contract_version.clone(),
                    }
                })?;
                let version = configuration
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| SchemaError::CompatibilityMissingConfiguration {
                        version: fixture.contract_version.clone(),
                    })?;
                let parsed = rootlight_config::ContractVersion::parse(version)
                    .map_err(|_| SchemaError::CompatibilityExpectedMajorRejection)?;
                if parsed.require_supported().is_ok() {
                    return Err(SchemaError::CompatibilityExpectedMajorRejection);
                }
            }
        }
    }

    validate_protobuf_unknown_field()?;
    println!("compatibility fixtures verified");
    Ok(())
}

fn validate_configuration_semantics(
    fixture_version: &str,
    configuration: &serde_json::Value,
) -> Result<(), SchemaError> {
    let version = configuration
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| SchemaError::CompatibilityMissingConfiguration {
            version: fixture_version.to_owned(),
        })?;
    if configuration
        .get("extensions")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|extensions| !extensions.is_empty())
    {
        return Err(SchemaError::CompatibilityExtensionFixtureUnsupported {
            version: fixture_version.to_owned(),
        });
    }

    let mut toml = format!("version = {version:?}\n");
    for section in ["security", "resources", "analysis"] {
        let Some(fields) = configuration
            .get(section)
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        toml.push_str(&format!("\n[{section}]\n"));
        for (key, value) in fields {
            let value = toml_scalar(value).ok_or_else(|| {
                SchemaError::CompatibilityConfigurationRejected {
                    version: fixture_version.to_owned(),
                }
            })?;
            toml.push_str(&format!("{key} = {value}\n"));
        }
    }

    rootlight_config::ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
        source: rootlight_config::ConfigSource::User,
        contents: &toml,
    }])
    .map_err(|_| SchemaError::CompatibilityConfigurationRejected {
        version: fixture_version.to_owned(),
    })?;
    Ok(())
}

fn toml_scalar(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => serde_json::to_string(value).ok(),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            None
        }
    }
}

fn validate_protobuf_unknown_field() -> Result<(), SchemaError> {
    let mut encoded = ProtocolContractVersion { major: 1, minor: 0 }.encode_to_vec();
    encoded.extend_from_slice(&[0x98, 0x06, 0x07]);
    let decoded = ProtocolContractVersion::decode(encoded.as_slice())
        .map_err(SchemaError::CompatibilityProtobufDecode)?;
    if decoded.major != 1 || decoded.minor != 0 {
        return Err(SchemaError::CompatibilityProtobufSemantics);
    }
    Ok(())
}

fn workspace_root() -> Result<PathBuf, SchemaError> {
    MetadataCommand::new()
        .no_deps()
        .exec()
        .map(|metadata| metadata.workspace_root.into_std_path_buf())
        .map_err(SchemaError::Metadata)
}

fn load_compatibility_fixtures(
    workspace_root: &Path,
) -> Result<Vec<CompatibilityFixture>, SchemaError> {
    COMPATIBILITY_FILES
        .iter()
        .map(|name| {
            let path = workspace_root.join(COMPATIBILITY_ROOT).join(name);
            let bytes = read_bytes(&path)?;
            serde_json::from_slice(&bytes)
                .map_err(|source| SchemaError::ParseCompatibilityFixture { path, source })
        })
        .collect()
}

fn generate_protobuf(workspace_root: &Path, staged_root: &Path) -> Result<(), SchemaError> {
    let proto_root = workspace_root.join("proto");
    let output_root = staged_root.join(PROTOCOL_GENERATED_ROOT);
    let descriptor_path = staged_root
        .join(SCHEMA_ROOT)
        .join("protobuf/rootlight.desc");
    create_parent(&descriptor_path)?;
    fs::create_dir_all(&output_root).map_err(|source| SchemaError::Write {
        path: output_root.clone(),
        source,
    })?;

    let protoc = protoc_bin_vendored::protoc_bin_path().map_err(SchemaError::VendoredProtoc)?;
    let include = protoc_bin_vendored::include_path().map_err(SchemaError::VendoredProtoc)?;
    let inputs: Vec<PathBuf> = PROTO_FILES
        .iter()
        .map(|name| proto_root.join(name))
        .collect();

    let mut config = prost_build::Config::new();
    config
        .protoc_executable(protoc)
        .out_dir(&output_root)
        .file_descriptor_set_path(&descriptor_path)
        .btree_map([".rootlight.common.v1.PublicError.details"])
        .field_attribute(".", "#[allow(missing_docs)]")
        .enum_attribute(".", "#[allow(missing_docs)]");
    config
        .compile_protos(&inputs, &[proto_root, include])
        .map_err(SchemaError::CompileProtobuf)?;

    let module = r#"// @generated by `cargo xtask generate`; do not edit.
//! Generated protobuf modules checked in for offline builds.

/// Common version, identity, error, and extension messages.
pub mod common {
    /// Version 1 common messages.
    pub mod v1 {
        include!("rootlight.common.v1.rs");
    }
}

/// Daemon protocol negotiation messages.
pub mod daemon {
    /// Version 1 daemon negotiation messages.
    pub mod v1 {
        include!("rootlight.daemon.v1.rs");
    }
}

/// Isolated adapter capability messages.
pub mod adapter {
    /// Version 1 adapter capability messages.
    pub mod v1 {
        include!("rootlight.adapter.v1.rs");
    }
}
"#;
    write_bytes(&output_root.join("mod.rs"), module.as_bytes())
}

fn generate_json_schemas(staged_root: &Path) -> Result<(), SchemaError> {
    let schema_root = staged_root.join(SCHEMA_ROOT).join("json");
    write_schema::<ConfigDocumentSchema>(&schema_root.join("config-1.0.schema.json"))?;
    write_schema::<IrDocumentSchema>(&schema_root.join("ir-1.0.schema.json"))?;
    write_schema::<ResponseMetadata>(&schema_root.join("mcp-response-metadata-1.0.schema.json"))?;
    write_schema::<ErrorResponse>(&schema_root.join("mcp-error-response-1.0.schema.json"))?;
    Ok(())
}

fn write_schema<T: JsonSchema>(path: &Path) -> Result<(), SchemaError> {
    let schema = SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator()
        .into_root_schema_for::<T>();
    let mut bytes = serde_json::to_vec_pretty(&schema).map_err(SchemaError::SerializeJson)?;
    bytes.push(b'\n');
    write_bytes(path, &bytes)
}

fn validate_generated_json_schemas(staged_root: &Path) -> Result<(), SchemaError> {
    let schema_root = staged_root.join(SCHEMA_ROOT).join("json");
    let cases = [
        (
            "config-1.0.schema.json",
            serde_json::json!({"version": "1.0"}),
            serde_json::json!({"version": "invalid"}),
        ),
        (
            "ir-1.0.schema.json",
            serde_json::json!({
                "version": {"major": 1, "minor": 0},
                "generation": "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by",
                "producer": {
                    "name": "fixture",
                    "version": "1.0",
                    "configuration_hash": "b3_rc6zkrxh5srdoiia2cydtoqh5ug2jyctujxicstuvgf2yz377y5zl6hbcu"
                },
                "build_context": {
                    "digest": "b3_rc6zkrxh5srdoiia2cydtoqh5ug2jyctujxicstuvgf2yz377y5zl6hbcu"
                },
                "coverage": "complete",
                "evidence": "syntax"
            }),
            serde_json::json!({
                "version": {"major": 1, "minor": 0},
                "generation": "invalid",
                "producer": {
                    "name": "fixture",
                    "version": "1.0",
                    "configuration_hash": "invalid"
                },
                "build_context": {"digest": "invalid"},
                "coverage": "anything",
                "evidence": "syntax"
            }),
        ),
        (
            "mcp-response-metadata-1.0.schema.json",
            serde_json::json!({
                "repository": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v",
                "generation": "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by",
                "coverage": "complete",
                "trust": "untrusted_repository_data"
            }),
            serde_json::json!({
                "repository": "not-an-id",
                "generation": "not-an-id",
                "coverage": "anything",
                "trust": "untrusted_repository_data"
            }),
        ),
        (
            "mcp-error-response-1.0.schema.json",
            serde_json::json!({
                "error": {
                    "code": "INTERNAL",
                    "message": "internal operation failed",
                    "retryable": false,
                    "retry_after_ms": null,
                    "repository": null,
                    "operation": null,
                    "generation": null,
                    "details": {},
                    "next_actions": []
                }
            }),
            serde_json::json!({
                "error": {
                    "code": "INTERNAL",
                    "message": "internal operation failed",
                    "retryable": false,
                    "retry_after_ms": null,
                    "repository": null,
                    "operation": null,
                    "generation": null,
                    "details": {},
                    "next_actions": [],
                    "secret": "must be rejected"
                }
            }),
        ),
    ];

    for (name, valid, invalid) in cases {
        let path = schema_root.join(name);
        let schema: serde_json::Value = serde_json::from_slice(&read_bytes(&path)?)
            .map_err(|source| SchemaError::ParseGeneratedJson { path, source })?;
        let validator = jsonschema::draft202012::new(&schema).map_err(|source| {
            SchemaError::CompileGeneratedSchema {
                name: name.to_owned(),
                detail: source.to_string(),
            }
        })?;
        if !validator.is_valid(&valid) || validator.is_valid(&invalid) {
            return Err(SchemaError::GeneratedSchemaSemantics(name.to_owned()));
        }
    }
    Ok(())
}

fn generate_manifest(workspace_root: &Path, staged_root: &Path) -> Result<(), SchemaError> {
    let artifacts = expected_artifact_paths();
    let package_versions = package_versions(workspace_root)?;
    let manifest = ArtifactManifest {
        schema_version: MANIFEST_VERSION,
        generators: GeneratorVersions {
            jsonschema: required_package_version(&package_versions, "jsonschema")?,
            prost: required_package_version(&package_versions, "prost")?,
            prost_build: required_package_version(&package_versions, "prost-build")?,
            protoc_bin_vendored: required_package_version(
                &package_versions,
                "protoc-bin-vendored",
            )?,
            schemars: required_package_version(&package_versions, "schemars")?,
        },
        inputs: generation_inputs(workspace_root)?,
        artifacts: artifacts
            .iter()
            .map(|path| {
                Ok(ArtifactRecord {
                    path: path.replace('\\', "/"),
                    blake3: digest_file(&staged_root.join(path))?,
                })
            })
            .collect::<Result<_, _>>()?,
    };
    let mut bytes = serde_json::to_vec_pretty(&manifest).map_err(SchemaError::SerializeJson)?;
    bytes.push(b'\n');
    write_bytes(&staged_root.join(SCHEMA_ROOT).join("manifest.json"), &bytes)
}

fn package_versions(workspace_root: &Path) -> Result<BTreeMap<String, String>, SchemaError> {
    let metadata = MetadataCommand::new()
        .current_dir(workspace_root)
        .exec()
        .map_err(SchemaError::Metadata)?;
    Ok(metadata
        .packages
        .into_iter()
        .map(|package| (package.name.to_string(), package.version.to_string()))
        .collect())
}

fn required_package_version<'a>(
    versions: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, SchemaError> {
    versions
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| SchemaError::MissingGeneratorPackage(name.to_owned()))
}

fn generation_inputs(workspace_root: &Path) -> Result<Vec<ArtifactRecord>, SchemaError> {
    let mut paths: Vec<String> = PROTO_FILES
        .iter()
        .map(|name| format!("proto/{name}"))
        .collect();
    paths.extend([
        "Cargo.lock".to_owned(),
        "Cargo.toml".to_owned(),
        "crates/rootlight-config/Cargo.toml".to_owned(),
        "crates/rootlight-config/src/lib.rs".to_owned(),
        "crates/rootlight-error/Cargo.toml".to_owned(),
        "crates/rootlight-error/src/lib.rs".to_owned(),
        "crates/rootlight-ids/Cargo.toml".to_owned(),
        "crates/rootlight-ids/src/lib.rs".to_owned(),
        "crates/rootlight-ir/Cargo.toml".to_owned(),
        "crates/rootlight-ir/src/lib.rs".to_owned(),
        "crates/rootlight-mcp-contract/Cargo.toml".to_owned(),
        "crates/rootlight-mcp-contract/src/lib.rs".to_owned(),
        "crates/rootlight-protocol/Cargo.toml".to_owned(),
        "xtask/Cargo.toml".to_owned(),
        "xtask/src/schemas.rs".to_owned(),
    ]);
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            Ok(ArtifactRecord {
                blake3: digest_file(&workspace_root.join(&path))?,
                path,
            })
        })
        .collect()
}

fn expected_artifact_paths() -> Vec<String> {
    let mut paths: Vec<String> = GENERATED_RUST_FILES
        .iter()
        .map(|name| format!("{PROTOCOL_GENERATED_ROOT}/{name}"))
        .collect();
    paths.extend([
        format!("{PROTOCOL_GENERATED_ROOT}/mod.rs"),
        format!("{SCHEMA_ROOT}/protobuf/rootlight.desc"),
        format!("{SCHEMA_ROOT}/json/config-1.0.schema.json"),
        format!("{SCHEMA_ROOT}/json/ir-1.0.schema.json"),
        format!("{SCHEMA_ROOT}/json/mcp-response-metadata-1.0.schema.json"),
        format!("{SCHEMA_ROOT}/json/mcp-error-response-1.0.schema.json"),
    ]);
    paths.sort();
    paths
}

fn update_outputs(workspace_root: &Path, staged_root: &Path) -> Result<(), SchemaError> {
    for relative in expected_output_paths() {
        let source = staged_root.join(&relative);
        let destination = workspace_root.join(&relative);
        let bytes = read_bytes(&source)?;
        write_bytes(&destination, &bytes)?;
    }
    remove_unexpected_outputs(workspace_root)
}

fn check_outputs(workspace_root: &Path, staged_root: &Path) -> Result<(), SchemaError> {
    let expected: BTreeSet<String> = expected_output_paths().into_iter().collect();
    let observed = observed_output_paths(workspace_root)?;
    if expected != observed {
        return Err(SchemaError::OutputSetMismatch {
            missing: expected.difference(&observed).cloned().collect(),
            unexpected: observed.difference(&expected).cloned().collect(),
        });
    }

    for relative in expected {
        let generated = read_bytes(&staged_root.join(&relative))?;
        let checked_in = read_bytes(&workspace_root.join(&relative))?;
        if generated != checked_in {
            return Err(SchemaError::Drift(relative));
        }
    }
    Ok(())
}

fn expected_output_paths() -> Vec<String> {
    let mut paths = expected_artifact_paths();
    paths.push(format!("{SCHEMA_ROOT}/manifest.json"));
    paths.sort();
    paths
}

fn observed_output_paths(workspace_root: &Path) -> Result<BTreeSet<String>, SchemaError> {
    let mut paths = BTreeSet::new();
    collect_files(
        workspace_root,
        &workspace_root.join(PROTOCOL_GENERATED_ROOT),
        &mut paths,
    )?;
    collect_files(
        workspace_root,
        &workspace_root.join(SCHEMA_ROOT),
        &mut paths,
    )?;
    Ok(paths)
}

fn collect_files(
    workspace_root: &Path,
    directory: &Path,
    paths: &mut BTreeSet<String>,
) -> Result<(), SchemaError> {
    if !directory.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(directory).map_err(|source| SchemaError::ReadDirectory {
        path: directory.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SchemaError::ReadDirectory {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| SchemaError::ReadDirectory {
                path: path.clone(),
                source,
            })?;
        if file_type.is_dir() {
            collect_files(workspace_root, &path, paths)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(workspace_root)
                .map_err(|_| SchemaError::PathOutsideWorkspace(path.clone()))?;
            paths.insert(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

fn remove_unexpected_outputs(workspace_root: &Path) -> Result<(), SchemaError> {
    let expected: BTreeSet<String> = expected_output_paths().into_iter().collect();
    for relative in observed_output_paths(workspace_root)? {
        if !expected.contains(&relative) {
            let path = workspace_root.join(&relative);
            fs::remove_file(&path).map_err(|source| SchemaError::Remove { path, source })?;
        }
    }
    Ok(())
}

fn digest_file(path: &Path) -> Result<String, SchemaError> {
    let bytes = read_bytes(path)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, SchemaError> {
    fs::read(path).map_err(|source| SchemaError::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), SchemaError> {
    create_parent(path)?;
    fs::write(path, bytes).map_err(|source| SchemaError::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn create_parent(path: &Path) -> Result<(), SchemaError> {
    let parent = path
        .parent()
        .ok_or_else(|| SchemaError::MissingParent(path.to_path_buf()))?;
    fs::create_dir_all(parent).map_err(|source| SchemaError::Write {
        path: parent.to_path_buf(),
        source,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompatibilityFixture {
    fixture_version: String,
    contract_version: String,
    disposition: FixtureDisposition,
    configuration: Option<serde_json::Value>,
    mcp_response_metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureDisposition {
    DraftLegacy,
    Production,
    UnsupportedMajor,
}

#[derive(Debug, Serialize)]
struct ArtifactManifest<'a> {
    schema_version: &'a str,
    generators: GeneratorVersions<'a>,
    inputs: Vec<ArtifactRecord>,
    artifacts: Vec<ArtifactRecord>,
}

#[derive(Debug, Serialize)]
struct GeneratorVersions<'a> {
    jsonschema: &'a str,
    prost: &'a str,
    prost_build: &'a str,
    protoc_bin_vendored: &'a str,
    schemars: &'a str,
}

#[derive(Debug, Serialize)]
struct ArtifactRecord {
    path: String,
    blake3: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SchemaError {
    #[error("failed to read Cargo metadata for schema generation")]
    Metadata(#[source] cargo_metadata::Error),
    #[error("schema generator package is missing from Cargo metadata: {0}")]
    MissingGeneratorPackage(String),
    #[error("failed to locate vendored protoc")]
    VendoredProtoc(#[source] protoc_bin_vendored::Error),
    #[error("failed to create temporary schema directory")]
    CreateTemporaryDirectory(#[source] std::io::Error),
    #[error("protobuf generation failed")]
    CompileProtobuf(#[source] std::io::Error),
    #[error("failed to serialize generated JSON")]
    SerializeJson(#[source] serde_json::Error),
    #[error("failed to parse generated JSON schema at {path}")]
    ParseGeneratedJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to compile generated JSON schema {name}: {detail}")]
    CompileGeneratedSchema { name: String, detail: String },
    #[error("SCHEMA_SEMANTICS: generated schema accepted or rejected the wrong fixture for {0}")]
    GeneratedSchemaSemantics(String),
    #[error("failed to parse compatibility fixture at {path}")]
    ParseCompatibilityFixture {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("COMPAT_FIXTURE_VERSION: unsupported compatibility fixture format")]
    UnsupportedCompatibilityFixtureVersion,
    #[error("COMPAT_FIXTURE_SET: expected 0.1, 0.2, 1.0, and 2.0 fixtures, observed {0:?}")]
    CompatibilityFixtureSet(Vec<String>),
    #[error("COMPAT_DISPOSITION: fixture {version} has an invalid compatibility disposition")]
    CompatibilityDispositionMismatch { version: String },
    #[error("COMPAT_CONFIG_MISSING: fixture {version} has no configuration contract")]
    CompatibilityMissingConfiguration { version: String },
    #[error("COMPAT_CONFIG_REJECTED: supported fixture {version} was rejected")]
    CompatibilityConfigurationRejected { version: String },
    #[error("COMPAT_EXTENSION_FIXTURE: fixture {version} uses unsupported extension payloads")]
    CompatibilityExtensionFixtureUnsupported { version: String },
    #[error("COMPAT_CONFIG_DECODE: configuration fixture {version} violates the wire contract")]
    CompatibilityConfigurationDecode {
        version: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("COMPAT_MCP_MISSING: fixture {version} has no MCP metadata contract")]
    CompatibilityMissingMcpMetadata { version: String },
    #[error("COMPAT_MCP_REJECTED: MCP metadata fixture {version} was rejected")]
    CompatibilityMcpRejected {
        version: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("COMPAT_PROTOBUF_DECODE: additive protobuf field fixture failed to decode")]
    CompatibilityProtobufDecode(#[source] prost::DecodeError),
    #[error("COMPAT_PROTOBUF_SEMANTICS: additive protobuf field changed known values")]
    CompatibilityProtobufSemantics,
    #[error("COMPAT_MAJOR_ACCEPTED: unsupported major fixture was accepted")]
    CompatibilityExpectedMajorRejection,
    #[error("failed to read generated artifact at {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to enumerate generated artifact directory at {path}")]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write generated artifact at {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to remove stale generated artifact at {path}")]
    Remove {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("generated artifact has no parent directory: {0}")]
    MissingParent(PathBuf),
    #[error("generated artifact is outside the workspace: {0}")]
    PathOutsideWorkspace(PathBuf),
    #[error("SCHEMA_OUTPUT_SET: missing {missing:?}, unexpected {unexpected:?}")]
    OutputSetMismatch {
        missing: Vec<String>,
        unexpected: Vec<String>,
    },
    #[error("SCHEMA_DRIFT: regenerate changed artifact {0}")]
    Drift(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_paths_are_unique_and_sorted() {
        let paths = expected_output_paths();
        let unique: BTreeSet<_> = paths.iter().collect();
        assert_eq!(paths.len(), unique.len());
        assert!(paths.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn manifest_records_every_non_manifest_artifact() {
        let outputs = expected_output_paths();
        let artifacts = expected_artifact_paths();
        assert_eq!(outputs.len(), artifacts.len() + 1);
        assert!(outputs.contains(&format!("{SCHEMA_ROOT}/manifest.json")));
        assert!(!artifacts.contains(&format!("{SCHEMA_ROOT}/manifest.json")));
    }
}
