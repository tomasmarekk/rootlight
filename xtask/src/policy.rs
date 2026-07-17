//! Enforces source, executable-dependency, action-pin, and unsafe-code policy.
//!
//! These checks use Cargo metadata and tracked configuration so they remain
//! deterministic after dependencies have been fetched and the network is blocked.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use cargo_metadata::{Metadata, MetadataCommand, Package, TargetKind};
use serde::Deserialize;
use sha2::Digest as _;
use yaml_rust2::{Yaml, YamlLoader};

const SUPPLY_CHAIN_POLICY_PATH: &str = "policy/supply-chain.toml";
const ACTION_POLICY_PATH: &str = "policy/github-actions.toml";
const TOOLCHAIN_POLICY_PATH: &str = "policy/toolchain.toml";
const WORKFLOW_ROOT: &str = ".github/workflows";
const CURRENT_SCHEMA_VERSION: &str = "1.0";

pub(crate) fn check() -> Result<(), PolicyError> {
    let metadata = MetadataCommand::new()
        .features(cargo_metadata::CargoOpt::AllFeatures)
        .other_options(vec!["--locked".to_owned()])
        .exec()
        .map_err(PolicyError::Metadata)?;
    let workspace_root = metadata.workspace_root.as_std_path();
    let supply_chain: SupplyChainPolicy =
        read_policy(&workspace_root.join(SUPPLY_CHAIN_POLICY_PATH))?;
    let action_policy: ActionPolicy = read_policy(&workspace_root.join(ACTION_POLICY_PATH))?;
    let toolchain_policy: ToolchainPolicy =
        read_policy(&workspace_root.join(TOOLCHAIN_POLICY_PATH))?;

    require_version(&supply_chain.schema_version, SUPPLY_CHAIN_POLICY_PATH)?;
    require_version(&action_policy.schema_version, ACTION_POLICY_PATH)?;
    require_version(&toolchain_policy.schema_version, TOOLCHAIN_POLICY_PATH)?;
    validate_dependency_surfaces(&metadata, &supply_chain)?;
    crate::grammar_lock::check(&metadata, workspace_root)
        .map_err(|error| PolicyError::GrammarLock(Box::new(error)))?;
    validate_action_pins(workspace_root, &action_policy)?;
    validate_toolchain_policy(workspace_root, &toolchain_policy)?;
    scan_workspace_unsafe(workspace_root, &metadata)?;

    println!(
        "policy check passed for {} resolved packages and {} approved actions",
        resolved_packages(&metadata).len(),
        action_policy.actions.len()
    );
    Ok(())
}

pub(crate) fn check_unsafe_fixture(root: &Path) -> Result<(), PolicyError> {
    scan_rust_tree(root)
}

fn read_policy<T>(path: &Path) -> Result<T, PolicyError>
where
    T: for<'de> Deserialize<'de>,
{
    let text = fs::read_to_string(path).map_err(|source| PolicyError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| PolicyError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn require_version(version: &str, path: &str) -> Result<(), PolicyError> {
    if version == CURRENT_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(PolicyError::UnsupportedPolicyVersion {
            path: path.to_owned(),
            version: version.to_owned(),
        })
    }
}

fn validate_dependency_surfaces(
    metadata: &Metadata,
    policy: &SupplyChainPolicy,
) -> Result<(), PolicyError> {
    let packages = resolved_packages(metadata);
    let workspace_ids: BTreeSet<_> = metadata.workspace_members.iter().collect();
    let allowed_registries = string_set(&policy.allowed_registries);
    let allowed_git = string_set(&policy.allowed_git_sources);
    let expected_build_scripts = string_set(&policy.allowed_build_scripts);
    let expected_proc_macros = string_set(&policy.allowed_proc_macros);
    let expected_native_links = string_set(&policy.allowed_native_links);

    let mut observed_build_scripts = BTreeSet::new();
    let mut observed_proc_macros = BTreeSet::new();
    let mut observed_native_links = BTreeSet::new();

    for package in &packages {
        if let Some(source) = &package.source {
            let source = source.to_string();
            if source.starts_with("registry+") {
                if !allowed_registries.contains(source.as_str()) {
                    return Err(PolicyError::UnapprovedRegistry {
                        package: package_key(package),
                        origin: source,
                    });
                }
            } else if source.starts_with("git+") {
                if !allowed_git.contains(source.as_str()) {
                    return Err(PolicyError::UnapprovedGitSource {
                        package: package_key(package),
                        origin: source,
                    });
                }
            } else {
                return Err(PolicyError::UnapprovedSource {
                    package: package_key(package),
                    origin: source,
                });
            }
        } else if !workspace_ids.contains(&package.id) {
            return Err(PolicyError::UnapprovedPathDependency {
                package: package_key(package),
                manifest: package.manifest_path.clone().into_std_path_buf(),
            });
        }

        let key = package_key(package);
        if package
            .targets
            .iter()
            .any(|target| target.kind.contains(&TargetKind::CustomBuild))
        {
            observed_build_scripts.insert(key.clone());
        }
        if package
            .targets
            .iter()
            .any(|target| target.kind.contains(&TargetKind::ProcMacro))
        {
            observed_proc_macros.insert(key.clone());
        }
        if let Some(links) = &package.links {
            observed_native_links.insert(format!("{key}:{links}"));
        }
    }

    compare_inventory(
        "build scripts",
        &expected_build_scripts,
        &observed_build_scripts,
    )?;
    compare_inventory(
        "procedural macros",
        &expected_proc_macros,
        &observed_proc_macros,
    )?;
    compare_inventory(
        "native links",
        &expected_native_links,
        &observed_native_links,
    )?;
    Ok(())
}

fn resolved_packages(metadata: &Metadata) -> Vec<&Package> {
    let resolved_ids: BTreeSet<_> = metadata
        .resolve
        .as_ref()
        .into_iter()
        .flat_map(|resolve| resolve.nodes.iter())
        .map(|node| &node.id)
        .collect();

    metadata
        .packages
        .iter()
        .filter(|package| resolved_ids.contains(&package.id))
        .collect()
}

fn compare_inventory(
    kind: &'static str,
    expected: &BTreeSet<&str>,
    observed: &BTreeSet<String>,
) -> Result<(), PolicyError> {
    let expected_owned: BTreeSet<String> =
        expected.iter().map(|value| (*value).to_owned()).collect();
    if expected_owned == *observed {
        return Ok(());
    }

    Err(PolicyError::InventoryMismatch {
        kind,
        missing: expected_owned.difference(observed).cloned().collect(),
        unexpected: observed.difference(&expected_owned).cloned().collect(),
    })
}

fn validate_action_pins(root: &Path, policy: &ActionPolicy) -> Result<(), PolicyError> {
    let approved: BTreeMap<&str, &str> = policy
        .actions
        .iter()
        .map(|action| (action.repository.as_str(), action.commit.as_str()))
        .collect();
    if approved.len() != policy.actions.len() {
        return Err(PolicyError::DuplicateActionPolicy);
    }

    let workflows = root.join(WORKFLOW_ROOT);
    let entries = fs::read_dir(&workflows).map_err(|source| PolicyError::Read {
        path: workflows.clone(),
        source,
    })?;
    let canonical_root = fs::canonicalize(root).map_err(|source| PolicyError::Read {
        path: root.to_path_buf(),
        source,
    })?;
    let mut inspected = BTreeSet::new();
    let mut used = BTreeSet::new();

    for entry in entries {
        let entry = entry.map_err(|source| PolicyError::Read {
            path: workflows.clone(),
            source,
        })?;
        let path = entry.path();
        if !matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|source| PolicyError::Read {
            path: path.clone(),
            source,
        })?;
        let documents =
            YamlLoader::load_from_str(&text).map_err(|source| PolicyError::WorkflowYaml {
                path: path.clone(),
                detail: source.to_string(),
            })?;
        if documents.len() != 1 {
            return Err(PolicyError::WorkflowDocumentCount {
                path: path.clone(),
                count: documents.len(),
            });
        }
        inspect_workflow_node(
            &canonical_root,
            &documents[0],
            &path,
            &approved,
            &mut used,
            &mut inspected,
        )?;
    }

    let unused: Vec<String> = approved
        .keys()
        .filter(|repository| !used.contains(**repository))
        .map(|repository| (*repository).to_owned())
        .collect();
    if unused.is_empty() {
        Ok(())
    } else {
        Err(PolicyError::UnusedActionPolicy(unused))
    }
}

fn inspect_workflow_node(
    root: &Path,
    node: &Yaml,
    path: &Path,
    approved: &BTreeMap<&str, &str>,
    used: &mut BTreeSet<String>,
    inspected: &mut BTreeSet<PathBuf>,
) -> Result<(), PolicyError> {
    match node {
        Yaml::Hash(mapping) => {
            for (key, value) in mapping {
                let Some(key) = key.as_str() else {
                    return Err(PolicyError::WorkflowKeyType(path.to_path_buf()));
                };
                match key {
                    "pull_request_target" => {
                        return Err(PolicyError::UnsafeWorkflowTrigger(path.to_path_buf()));
                    }
                    "container" | "services" => {
                        return Err(PolicyError::WorkflowContainer {
                            path: path.to_path_buf(),
                            key: key.to_owned(),
                        });
                    }
                    "uses" => {
                        validate_action_reference(root, value, path, approved, used, inspected)?
                    }
                    "permissions" => validate_permissions(value, path)?,
                    _ => {}
                }
                inspect_workflow_node(root, value, path, approved, used, inspected)?;
            }
        }
        Yaml::Array(values) => {
            for value in values {
                inspect_workflow_node(root, value, path, approved, used, inspected)?;
            }
        }
        Yaml::Alias(_) => return Err(PolicyError::WorkflowAlias(path.to_path_buf())),
        Yaml::Real(_)
        | Yaml::Integer(_)
        | Yaml::String(_)
        | Yaml::Boolean(_)
        | Yaml::Null
        | Yaml::BadValue => {}
    }
    Ok(())
}

fn validate_action_reference(
    root: &Path,
    value: &Yaml,
    path: &Path,
    approved: &BTreeMap<&str, &str>,
    used: &mut BTreeSet<String>,
    inspected: &mut BTreeSet<PathBuf>,
) -> Result<(), PolicyError> {
    let Some(action_ref) = value.as_str() else {
        return Err(PolicyError::WorkflowActionType(path.to_path_buf()));
    };
    if action_ref.starts_with("./") {
        return validate_local_action(root, action_ref, approved, used, inspected);
    }
    let Some((repository, commit)) = action_ref.rsplit_once('@') else {
        return Err(PolicyError::UnpinnedAction {
            path: path.to_path_buf(),
            action: action_ref.to_owned(),
        });
    };
    if !is_commit_sha(commit) {
        return Err(PolicyError::UnpinnedAction {
            path: path.to_path_buf(),
            action: action_ref.to_owned(),
        });
    }
    match approved.get(repository) {
        Some(expected) if *expected == commit => {
            used.insert(repository.to_owned());
            Ok(())
        }
        Some(expected) => Err(PolicyError::ActionCommitMismatch {
            repository: repository.to_owned(),
            expected: (*expected).to_owned(),
            observed: commit.to_owned(),
        }),
        None => Err(PolicyError::UnapprovedAction(repository.to_owned())),
    }
}

fn validate_local_action(
    root: &Path,
    action_ref: &str,
    approved: &BTreeMap<&str, &str>,
    used: &mut BTreeSet<String>,
    inspected: &mut BTreeSet<PathBuf>,
) -> Result<(), PolicyError> {
    let relative = action_ref
        .strip_prefix("./")
        .expect("local action references are checked before validation");
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(PolicyError::InvalidLocalActionReference(
            action_ref.to_owned(),
        ));
    }
    let action_root =
        fs::canonicalize(root.join(relative)).map_err(|source| PolicyError::Read {
            path: root.join(relative),
            source,
        })?;
    if !action_root.starts_with(root) {
        return Err(PolicyError::InvalidLocalActionReference(
            action_ref.to_owned(),
        ));
    }
    let candidates = [
        action_root.join("action.yml"),
        action_root.join("action.yaml"),
    ];
    let Some(path) = candidates.iter().find(|candidate| candidate.is_file()) else {
        return Err(PolicyError::LocalActionMetadata(action_root));
    };
    let path = fs::canonicalize(path).map_err(|source| PolicyError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if !path.starts_with(root) {
        return Err(PolicyError::InvalidLocalActionReference(
            action_ref.to_owned(),
        ));
    }
    if !inspected.insert(path.clone()) {
        return Ok(());
    }
    let text = fs::read_to_string(&path).map_err(|source| PolicyError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let documents =
        YamlLoader::load_from_str(&text).map_err(|source| PolicyError::WorkflowYaml {
            path: path.to_path_buf(),
            detail: source.to_string(),
        })?;
    if documents.len() != 1 {
        return Err(PolicyError::WorkflowDocumentCount {
            path: path.to_path_buf(),
            count: documents.len(),
        });
    }
    inspect_workflow_node(root, &documents[0], &path, approved, used, inspected)
}

fn validate_permissions(value: &Yaml, path: &Path) -> Result<(), PolicyError> {
    match value {
        Yaml::Null => Ok(()),
        Yaml::String(permission) if permission == "read-all" => Ok(()),
        Yaml::Hash(mapping) => {
            for permission in mapping.values() {
                match permission.as_str() {
                    Some("read") | Some("none") => {}
                    _ => return Err(PolicyError::WorkflowWritePermission(path.to_path_buf())),
                }
            }
            Ok(())
        }
        _ => Err(PolicyError::WorkflowWritePermission(path.to_path_buf())),
    }
}

fn is_commit_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_toolchain_policy(root: &Path, policy: &ToolchainPolicy) -> Result<(), PolicyError> {
    let mut names = BTreeSet::new();
    for item in policy.inputs.iter().chain(&policy.tools) {
        if !names.insert(item.name.as_str()) {
            return Err(PolicyError::DuplicateToolchainEntry(item.name.clone()));
        }
        validate_https_url(&item.url, &item.name)?;
        validate_sha256(&item.sha256, &item.name)?;
        if let Some(lockfile) = &item.lockfile {
            let Some(expected) = &item.lockfile_sha256 else {
                return Err(PolicyError::MissingToolchainLockDigest(item.name.clone()));
            };
            validate_sha256(expected, &item.name)?;
            let path = root.join(lockfile);
            let bytes = fs::read(&path).map_err(|source| PolicyError::Read {
                path: path.clone(),
                source,
            })?;
            let observed = sha256_hex(&bytes);
            if observed != *expected {
                return Err(PolicyError::ToolchainLockDigest {
                    name: item.name.clone(),
                    expected: expected.clone(),
                    observed,
                });
            }
        } else if item.lockfile_sha256.is_some() {
            return Err(PolicyError::UnexpectedToolchainLockDigest(
                item.name.clone(),
            ));
        }
    }
    if policy.inputs.is_empty() || policy.tools.is_empty() {
        return Err(PolicyError::EmptyToolchainPolicy);
    }
    Ok(())
}

fn validate_https_url(url: &str, name: &str) -> Result<(), PolicyError> {
    if url.starts_with("https://") && !url.chars().any(char::is_whitespace) {
        Ok(())
    } else {
        Err(PolicyError::ToolchainUrl {
            name: name.to_owned(),
            url: url.to_owned(),
        })
    }
}

fn validate_sha256(value: &str, name: &str) -> Result<(), PolicyError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(PolicyError::ToolchainDigest(name.to_owned()))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = sha2::Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

fn scan_workspace_unsafe(root: &Path, metadata: &Metadata) -> Result<(), PolicyError> {
    for package in metadata.workspace_packages() {
        for target in &package.targets {
            let source = target.src_path.as_std_path();
            if let Some(source_root) = source.parent() {
                scan_rust_tree(source_root)?;
            }
        }
    }
    scan_rust_tree(&root.join("tests/fixtures/unsafe"))
}

fn scan_rust_tree(root: &Path) -> Result<(), PolicyError> {
    if !root.exists() {
        return Ok(());
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|source| PolicyError::Read {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| PolicyError::Read {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| PolicyError::Read {
                path: path.clone(),
                source,
            })?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file()
                && path.extension().and_then(|value| value.to_str()) == Some("rs")
            {
                scan_rust_file(&path)?;
            }
        }
    }
    Ok(())
}

fn scan_rust_file(path: &Path) -> Result<(), PolicyError> {
    let text = fs::read_to_string(path).map_err(|source| PolicyError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if starts_line_comment(bytes, index) {
            index = skip_line_comment(bytes, index + 2);
        } else if starts_block_comment(bytes, index) {
            index = skip_block_comment(bytes, index + 2);
        } else if let Some(end) = skip_raw_string(bytes, index) {
            index = end;
        } else if let Some((quote_index, quote)) = quoted_literal_start(bytes, index) {
            index = skip_quoted(bytes, quote_index, quote);
        } else if is_identifier_start(bytes[index]) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_identifier_continue(bytes[index]) {
                index += 1;
            }
            if &bytes[start..index] == b"unsafe" {
                return Err(PolicyError::UnsafeToken {
                    path: path.to_path_buf(),
                    line: line_number(bytes, start),
                });
            }
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn starts_line_comment(bytes: &[u8], index: usize) -> bool {
    bytes.get(index..index + 2) == Some(b"//")
}

fn starts_block_comment(bytes: &[u8], index: usize) -> bool {
    bytes.get(index..index + 2) == Some(b"/*")
}

fn skip_line_comment(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index] != b'\n' {
        index += 1;
    }
    index
}

fn skip_block_comment(bytes: &[u8], mut index: usize) -> usize {
    let mut depth = 1_u32;
    while index < bytes.len() && depth > 0 {
        if starts_block_comment(bytes, index) {
            depth = depth.saturating_add(1);
            index += 2;
        } else if bytes.get(index..index + 2) == Some(b"*/") {
            depth -= 1;
            index += 2;
        } else {
            index += 1;
        }
    }
    index
}

fn quoted_literal_start(bytes: &[u8], index: usize) -> Option<(usize, u8)> {
    match bytes.get(index).copied()? {
        b'"' => Some((index, b'"')),
        b'\'' if is_character_literal(bytes, index) => Some((index, b'\'')),
        b'b' if bytes.get(index + 1) == Some(&b'"') => Some((index + 1, b'"')),
        b'b' if bytes.get(index + 1) == Some(&b'\'') && is_character_literal(bytes, index + 1) => {
            Some((index + 1, b'\''))
        }
        b'c' if bytes.get(index + 1) == Some(&b'"') => Some((index + 1, b'"')),
        _ => None,
    }
}

fn is_character_literal(bytes: &[u8], quote_index: usize) -> bool {
    let mut index = quote_index + 1;
    if bytes.get(index) == Some(&b'\\') {
        index += 1;
        match bytes.get(index).copied() {
            Some(b'x') => index += 3,
            Some(b'u') if bytes.get(index + 1) == Some(&b'{') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'}' {
                    index += 1;
                }
                index += usize::from(index < bytes.len());
            }
            Some(_) => index += 1,
            None => return false,
        }
    } else {
        let Some(first) = bytes.get(index).copied() else {
            return false;
        };
        let width = utf8_width(first);
        if width == 0 || index + width > bytes.len() {
            return false;
        }
        index += width;
    }
    bytes.get(index) == Some(&b'\'')
}

fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 0,
    }
}

fn skip_raw_string(bytes: &[u8], index: usize) -> Option<usize> {
    let mut cursor = index;
    if matches!(bytes.get(cursor), Some(b'b' | b'c')) {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hashes_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    let hashes = cursor - hashes_start;
    cursor += 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes.get(cursor + 1..cursor + 1 + hashes)
                == Some(&bytes[hashes_start..hashes_start + hashes])
        {
            return Some(cursor + 1 + hashes);
        }
        cursor += 1;
    }
    Some(bytes.len())
}

fn skip_quoted(bytes: &[u8], mut index: usize, quote: u8) -> usize {
    index += 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index = (index + 2).min(bytes.len()),
            value if value == quote => return index + 1,
            _ => index += 1,
        }
    }
    index
}

fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit()
}

fn line_number(bytes: &[u8], index: usize) -> usize {
    bytes[..index].iter().filter(|byte| **byte == b'\n').count() + 1
}

fn string_set(values: &[String]) -> BTreeSet<&str> {
    values.iter().map(String::as_str).collect()
}

fn package_key(package: &Package) -> String {
    format!("{}@{}", package.name, package.version)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupplyChainPolicy {
    schema_version: String,
    allowed_registries: Vec<String>,
    allowed_git_sources: Vec<String>,
    allowed_build_scripts: Vec<String>,
    allowed_proc_macros: Vec<String>,
    allowed_native_links: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionPolicy {
    schema_version: String,
    actions: Vec<ActionPin>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionPin {
    repository: String,
    commit: String,
    #[serde(rename = "release")]
    _release: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolchainPolicy {
    schema_version: String,
    inputs: Vec<ToolchainItem>,
    tools: Vec<ToolchainItem>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolchainItem {
    name: String,
    url: String,
    sha256: String,
    #[serde(default)]
    lockfile: Option<PathBuf>,
    #[serde(default)]
    lockfile_sha256: Option<String>,
    #[serde(default, rename = "version")]
    _version: Option<String>,
    #[serde(default, rename = "revision")]
    _revision: Option<String>,
    #[serde(default, rename = "generation")]
    _generation: Option<String>,
    #[serde(default, rename = "updated")]
    _updated: Option<String>,
    #[serde(default, rename = "install")]
    _install: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PolicyError {
    #[error(transparent)]
    GrammarLock(Box<crate::grammar_lock::GrammarLockError>),
    #[error("failed to read policy input at {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse policy input at {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to read Cargo metadata")]
    Metadata(#[source] cargo_metadata::Error),
    #[error("POLICY_VERSION: unsupported version {version} in {path}")]
    UnsupportedPolicyVersion { path: String, version: String },
    #[error("POLICY_SOURCE_REGISTRY: {package} uses unapproved registry {origin}")]
    UnapprovedRegistry { package: String, origin: String },
    #[error("POLICY_SOURCE_GIT: {package} uses unapproved Git source {origin}")]
    UnapprovedGitSource { package: String, origin: String },
    #[error("POLICY_SOURCE_KIND: {package} uses unapproved source {origin}")]
    UnapprovedSource { package: String, origin: String },
    #[error("POLICY_SOURCE_PATH: {package} uses unapproved path dependency {manifest}")]
    UnapprovedPathDependency { package: String, manifest: PathBuf },
    #[error("POLICY_INVENTORY: {kind} mismatch; missing {missing:?}, unexpected {unexpected:?}")]
    InventoryMismatch {
        kind: &'static str,
        missing: Vec<String>,
        unexpected: Vec<String>,
    },
    #[error("POLICY_ACTION_DUPLICATE: action policy contains a duplicate repository")]
    DuplicateActionPolicy,
    #[error("POLICY_WORKFLOW_YAML: failed to parse {path}: {detail}")]
    WorkflowYaml { path: PathBuf, detail: String },
    #[error("POLICY_WORKFLOW_DOCUMENTS: {path} contains {count} YAML documents")]
    WorkflowDocumentCount { path: PathBuf, count: usize },
    #[error("POLICY_WORKFLOW_KEY: {0} contains a non-string mapping key")]
    WorkflowKeyType(PathBuf),
    #[error("POLICY_WORKFLOW_ALIAS: {0} contains a YAML alias")]
    WorkflowAlias(PathBuf),
    #[error("POLICY_WORKFLOW_ACTION: {0} contains a non-string uses value")]
    WorkflowActionType(PathBuf),
    #[error("POLICY_ACTION_LOCAL_REFERENCE: invalid local action reference {0}")]
    InvalidLocalActionReference(String),
    #[error("POLICY_ACTION_LOCAL_METADATA: local action at {0} has no action.yml or action.yaml")]
    LocalActionMetadata(PathBuf),
    #[error("POLICY_ACTION_UNPINNED: {path} uses mutable action reference {action}")]
    UnpinnedAction { path: PathBuf, action: String },
    #[error("POLICY_ACTION_UNAPPROVED: workflow uses unapproved action {0}")]
    UnapprovedAction(String),
    #[error("POLICY_ACTION_COMMIT: {repository} expected {expected}, observed {observed}")]
    ActionCommitMismatch {
        repository: String,
        expected: String,
        observed: String,
    },
    #[error("POLICY_ACTION_UNUSED: approved actions are unused: {0:?}")]
    UnusedActionPolicy(Vec<String>),
    #[error("POLICY_WORKFLOW_TRIGGER: {0} uses pull_request_target")]
    UnsafeWorkflowTrigger(PathBuf),
    #[error("POLICY_WORKFLOW_CONTAINER: {path} uses unpinned {key} configuration")]
    WorkflowContainer { path: PathBuf, key: String },
    #[error("POLICY_WORKFLOW_PERMISSION: {0} grants write permission")]
    WorkflowWritePermission(PathBuf),
    #[error("POLICY_TOOLCHAIN_DUPLICATE: duplicate tool or input {0}")]
    DuplicateToolchainEntry(String),
    #[error("POLICY_TOOLCHAIN_EMPTY: toolchain policy requires inputs and tools")]
    EmptyToolchainPolicy,
    #[error("POLICY_TOOLCHAIN_URL: {name} uses invalid URL {url}")]
    ToolchainUrl { name: String, url: String },
    #[error("POLICY_TOOLCHAIN_DIGEST: {0} has an invalid SHA-256 digest")]
    ToolchainDigest(String),
    #[error("POLICY_TOOLCHAIN_LOCK: {0} is missing its lockfile digest")]
    MissingToolchainLockDigest(String),
    #[error("POLICY_TOOLCHAIN_LOCK: {0} has a digest without a lockfile")]
    UnexpectedToolchainLockDigest(String),
    #[error("POLICY_TOOLCHAIN_LOCK: {name} expected {expected}, observed {observed}")]
    ToolchainLockDigest {
        name: String,
        expected: String,
        observed: String,
    },
    #[error("POLICY_UNSAFE: unsafe token in {path}:{line}")]
    UnsafeToken { path: PathBuf, line: usize },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn unsafe_scan_ignores_comments_and_literals() {
        let directory = tempdir().expect("temporary directory is available");
        let source = directory.path().join("safe.rs");
        fs::write(
            &source,
            "// unsafe block\nconst LABEL: &str = \"unsafe\";\nconst RAW: &str = r#\"unsafe { ignored }\"#;\nconst BYTES: &[u8] = br\"unsafe\";\nconst C_BYTES: &CStr = cr#\"unsafe\"#;\nfn safe() {}\n",
        )
        .expect("fixture writes");

        assert!(scan_rust_file(&source).is_ok());
    }

    #[test]
    fn unsafe_scan_rejects_code_tokens() {
        let directory = tempdir().expect("temporary directory is available");
        let source = directory.path().join("unsafe.rs");
        fs::write(
            &source,
            "fn fixture() { unsafe { core::hint::unreachable_unchecked() } }\n",
        )
        .expect("fixture writes");

        assert!(matches!(
            scan_rust_file(&source),
            Err(PolicyError::UnsafeToken { line: 1, .. })
        ));
    }

    #[test]
    fn unsafe_scan_does_not_treat_labels_as_character_literals() {
        let directory = tempdir().expect("temporary directory is available");
        let source = directory.path().join("unsafe.rs");
        fs::write(
            &source,
            "fn fixture() { 'scan: loop { unsafe { core::hint::unreachable_unchecked() } break 'scan; } }\n",
        )
        .expect("fixture writes");

        assert!(matches!(
            scan_rust_file(&source),
            Err(PolicyError::UnsafeToken { line: 1, .. })
        ));
    }

    #[test]
    fn unsafe_scan_closes_comments_before_code() {
        let directory = tempdir().expect("temporary directory is available");
        let source = directory.path().join("unsafe.rs");
        fs::write(
            &source,
            "fn fixture() { /* \" */ unsafe { core::hint::unreachable_unchecked() } }\n",
        )
        .expect("fixture writes");

        assert!(matches!(
            scan_rust_file(&source),
            Err(PolicyError::UnsafeToken { line: 1, .. })
        ));
    }

    #[test]
    fn workflow_parser_rejects_flow_syntax_bypasses() {
        let documents = YamlLoader::load_from_str(
            "on: { pull_request_target: null }\npermissions: { contents: \"write\" }\n",
        )
        .expect("fixture parses");
        let approved = BTreeMap::new();
        let mut inspected = BTreeSet::new();
        let mut used = BTreeSet::new();

        assert!(matches!(
            inspect_workflow_node(
                Path::new("."),
                &documents[0],
                Path::new("fixture.yml"),
                &approved,
                &mut used,
                &mut inspected,
            ),
            Err(PolicyError::UnsafeWorkflowTrigger(_))
        ));
    }

    #[test]
    fn workflow_parser_accepts_local_and_pinned_actions() {
        let directory = tempdir().expect("temporary directory is available");
        let local_action = directory.path().join("local-action");
        fs::create_dir(&local_action).expect("local action directory creates");
        let commit = "0123456789abcdef0123456789abcdef01234567";
        fs::write(
            local_action.join("action.yml"),
            format!(
                "name: fixture\nruns:\n  using: composite\n  steps:\n    - uses: vendor/action@{commit}\n"
            ),
        )
        .expect("local action metadata writes");
        let documents = YamlLoader::load_from_str(&format!(
            "permissions: {{ contents: read }}\njobs:\n  local:\n    uses: ./local-action\n  external:\n    uses : vendor/action@{commit}\n"
        ))
        .expect("fixture parses");
        let approved = BTreeMap::from([("vendor/action", commit)]);
        let root = fs::canonicalize(directory.path()).expect("temporary root canonicalizes");
        let mut inspected = BTreeSet::new();
        let mut used = BTreeSet::new();

        inspect_workflow_node(
            &root,
            &documents[0],
            Path::new("fixture.yml"),
            &approved,
            &mut used,
            &mut inspected,
        )
        .expect("approved actions pass");
        assert_eq!(used, BTreeSet::from(["vendor/action".to_owned()]));
    }

    #[test]
    fn workflow_parser_rejects_mutable_actions_in_local_composites() {
        let directory = tempdir().expect("temporary directory is available");
        let local_action = directory.path().join("local-action");
        fs::create_dir(&local_action).expect("local action directory creates");
        fs::write(
            local_action.join("action.yml"),
            "name: fixture\nruns:\n  using: composite\n  steps:\n    - uses: vendor/action@main\n",
        )
        .expect("local action metadata writes");
        let documents = YamlLoader::load_from_str("jobs:\n  local:\n    uses: ./local-action\n")
            .expect("fixture parses");
        let approved = BTreeMap::new();
        let root = fs::canonicalize(directory.path()).expect("temporary root canonicalizes");
        let mut inspected = BTreeSet::new();
        let mut used = BTreeSet::new();

        assert!(matches!(
            inspect_workflow_node(
                &root,
                &documents[0],
                Path::new("fixture.yml"),
                &approved,
                &mut used,
                &mut inspected,
            ),
            Err(PolicyError::UnpinnedAction { .. })
        ));
    }
}
