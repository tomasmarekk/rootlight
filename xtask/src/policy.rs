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
use syn::{
    AttrStyle, Attribute, Expr, Item, Lit, LitStr, Meta, Path as SynPath, UseTree,
    visit::{self, Visit},
};
use yaml_rust2::{Yaml, YamlLoader};

const SUPPLY_CHAIN_POLICY_PATH: &str = "policy/supply-chain.toml";
const ACTION_POLICY_PATH: &str = "policy/github-actions.toml";
const TOOLCHAIN_POLICY_PATH: &str = "policy/toolchain.toml";
const UNSAFE_POLICY_PATH: &str = "policy/unsafe.toml";
const WORKFLOW_ROOT: &str = ".github/workflows";
const CURRENT_SCHEMA_VERSION: &str = "1.0";
const ACCEPTED_UNSAFE_EVIDENCE_UNIMPLEMENTED: &str = "Accepted unsafe boundary evidence requires compiler-derived expanded input inventory and the full cargo-geiger SafetyReport; this evidence is not implemented";

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
    let unsafe_policy: UnsafePolicy = read_policy(&workspace_root.join(UNSAFE_POLICY_PATH))?;

    require_version(&supply_chain.schema_version, SUPPLY_CHAIN_POLICY_PATH)?;
    require_version(&action_policy.schema_version, ACTION_POLICY_PATH)?;
    require_version(&toolchain_policy.schema_version, TOOLCHAIN_POLICY_PATH)?;
    require_version(&unsafe_policy.schema_version, UNSAFE_POLICY_PATH)?;
    validate_dependency_surfaces(&metadata, &supply_chain)?;
    crate::grammar_lock::check(&metadata, workspace_root)
        .map_err(|error| PolicyError::GrammarLock(Box::new(error)))?;
    validate_action_pins(workspace_root, &action_policy)?;
    validate_toolchain_policy(workspace_root, &toolchain_policy)?;
    scan_workspace_unsafe(workspace_root, &metadata, &unsafe_policy)?;

    println!(
        "policy check passed for {} resolved packages and {} approved actions",
        resolved_packages(&metadata).len(),
        action_policy.actions.len()
    );
    Ok(())
}

pub(crate) fn check_unsafe_fixture(root: &Path) -> Result<(), PolicyError> {
    let mut observed = BTreeMap::new();
    scan_rust_tree(root, &mut observed)?;
    if let Some((path, observation)) = observed
        .into_iter()
        .find(|(_, observation)| observation.count > 0)
    {
        return Err(PolicyError::UnsafeToken {
            path,
            line: observation.first_line,
        });
    }
    Ok(())
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

fn scan_workspace_unsafe(
    root: &Path,
    metadata: &Metadata,
    policy: &UnsafePolicy,
) -> Result<(), PolicyError> {
    reject_accepted_boundaries_without_authoritative_evidence(policy)?;
    let root = fs::canonicalize(root).map_err(|source| PolicyError::Read {
        path: root.to_path_buf(),
        source,
    })?;
    let boundaries = validate_unsafe_boundaries(&root, metadata, policy)?;
    let mut observed = BTreeMap::new();
    for package in metadata.workspace_packages() {
        let Some(manifest_parent) = package.manifest_path.as_std_path().parent() else {
            return Err(PolicyError::InvalidUnsafeBoundary {
                detail: format!("package {} has no manifest parent", package.name),
            });
        };
        let package_root =
            fs::canonicalize(manifest_parent).map_err(|source| PolicyError::Read {
                path: manifest_parent.to_path_buf(),
                source,
            })?;
        if !package_root.starts_with(&root) {
            return Err(PolicyError::InvalidUnsafeBoundary {
                detail: format!("workspace package {} escapes the workspace", package.name),
            });
        }
        scan_rust_tree(&package_root, &mut observed)?;
    }
    scan_rust_tree(&root.join("tests/fixtures/unsafe"), &mut observed)?;

    for (path, observation) in &observed {
        if observation.count == 0 {
            continue;
        }
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| PolicyError::InvalidUnsafeBoundary {
                detail: format!("workspace source is outside root: {}", path.display()),
            })?
            .to_path_buf();
        let Some(boundary) = boundaries.get(&relative) else {
            return Err(PolicyError::UnsafeToken {
                path: path.clone(),
                line: observation.first_line,
            });
        };
        if boundary.status != UnsafeBoundaryStatus::Accepted {
            return Err(PolicyError::UnsafeToken {
                path: path.clone(),
                line: observation.first_line,
            });
        }
    }

    for (source, boundary) in boundaries {
        let absolute = root.join(&source);
        let observation = observed.get(&absolute).copied().unwrap_or_default();
        let expected = boundary.expected_source_tokens;
        match boundary.status {
            UnsafeBoundaryStatus::Proposed if expected != 0 || observation.count != 0 => {
                return Err(PolicyError::UnsafeBoundaryCount {
                    path: source,
                    expected: 0,
                    observed: observation.count,
                });
            }
            UnsafeBoundaryStatus::Accepted if expected == 0 => {
                return Err(PolicyError::InvalidUnsafeBoundary {
                    detail: format!(
                        "accepted boundary {} must expect at least one token",
                        source.display()
                    ),
                });
            }
            UnsafeBoundaryStatus::Accepted | UnsafeBoundaryStatus::Proposed
                if observation.count != expected =>
            {
                return Err(PolicyError::UnsafeBoundaryCount {
                    path: source,
                    expected,
                    observed: observation.count,
                });
            }
            UnsafeBoundaryStatus::Accepted | UnsafeBoundaryStatus::Proposed => {}
        }
    }
    Ok(())
}

fn reject_accepted_boundaries_without_authoritative_evidence(
    policy: &UnsafePolicy,
) -> Result<(), PolicyError> {
    if policy
        .boundaries
        .iter()
        .any(|boundary| boundary.status == UnsafeBoundaryStatus::Accepted)
    {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: ACCEPTED_UNSAFE_EVIDENCE_UNIMPLEMENTED.to_owned(),
        });
    }
    Ok(())
}

fn validate_unsafe_boundaries<'a>(
    root: &Path,
    metadata: &Metadata,
    policy: &'a UnsafePolicy,
) -> Result<BTreeMap<PathBuf, &'a UnsafeBoundary>, PolicyError> {
    let mut by_source = BTreeMap::new();
    let mut modules = BTreeSet::new();
    for boundary in &policy.boundaries {
        validate_relative_policy_path(&boundary.source)?;
        validate_relative_policy_path(&boundary.adr)?;
        validate_relative_policy_path(&boundary.manifest)?;
        if boundary.module.is_empty()
            || boundary.owner != "@tomasmarekk"
            || boundary.reason.trim().is_empty()
        {
            return Err(PolicyError::InvalidUnsafeBoundary {
                detail: format!("incomplete boundary for {}", boundary.source.display()),
            });
        }
        let absolute_manifest =
            canonical_policy_file(root, &boundary.manifest, "unsafe boundary manifest")?;
        let matching_packages = metadata
            .workspace_packages()
            .iter()
            .filter(|package| {
                package.name == boundary.package
                    && package.version.to_string() == boundary.package_version
                    && fs::canonicalize(package.manifest_path.as_std_path())
                        .is_ok_and(|manifest| manifest == absolute_manifest)
            })
            .copied()
            .collect::<Vec<_>>();
        let [package] = matching_packages.as_slice() else {
            return Err(PolicyError::InvalidUnsafeBoundary {
                detail: format!(
                    "boundary package identity must match exactly one workspace member: {}@{} ({})",
                    boundary.package,
                    boundary.package_version,
                    boundary.manifest.display()
                ),
            });
        };
        let Some(manifest_parent) = package.manifest_path.as_std_path().parent() else {
            return Err(PolicyError::InvalidUnsafeBoundary {
                detail: format!("package {} has no manifest parent", boundary.package),
            });
        };
        let package_root =
            fs::canonicalize(manifest_parent).map_err(|source| PolicyError::Read {
                path: manifest_parent.to_path_buf(),
                source,
            })?;
        let absolute_source =
            canonical_policy_file(root, &boundary.source, "unsafe boundary source")?;
        let absolute_adr = canonical_policy_file(root, &boundary.adr, "unsafe boundary ADR")?;
        if !absolute_source.starts_with(&package_root)
            || absolute_source.extension().and_then(|value| value.to_str()) != Some("rs")
        {
            return Err(PolicyError::InvalidUnsafeBoundary {
                detail: format!(
                    "{} is stale or outside package {}",
                    boundary.source.display(),
                    boundary.package
                ),
            });
        }
        validate_unsafe_boundary_governance(
            package,
            &package_root,
            &absolute_source,
            &absolute_adr,
            boundary,
        )?;
        if !modules.insert(boundary.module.as_str())
            || by_source
                .insert(boundary.source.clone(), boundary)
                .is_some()
        {
            return Err(PolicyError::DuplicateUnsafeBoundary {
                path: boundary.source.clone(),
            });
        }
    }
    Ok(by_source)
}

fn canonical_policy_file(
    root: &Path,
    relative: &Path,
    kind: &'static str,
) -> Result<PathBuf, PolicyError> {
    let requested = root.join(relative);
    let canonical = fs::canonicalize(&requested).map_err(|source| PolicyError::Read {
        path: requested.clone(),
        source,
    })?;
    if !canonical.starts_with(root)
        || canonical
            .strip_prefix(root)
            .is_ok_and(|observed| observed != relative)
        || !canonical.is_file()
    {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: format!("{kind} escapes or aliases {}", relative.display()),
        });
    }
    Ok(canonical)
}

struct LibraryModuleGraph {
    target_source: PathBuf,
    modules: BTreeMap<String, PathBuf>,
}

fn package_target_defining_module(
    package: &Package,
    package_root: &Path,
    module: &str,
    source: &Path,
) -> Result<Option<LibraryModuleGraph>, PolicyError> {
    for target in package
        .targets
        .iter()
        .filter(|target| target.kind.contains(&TargetKind::Lib))
    {
        let target_path = target.src_path.as_std_path();
        if path_is_link_or_reparse(target_path)? {
            return Err(PolicyError::InvalidUnsafeBoundary {
                detail: format!(
                    "Cargo target is a symlink or reparse point: {}",
                    target_path.display()
                ),
            });
        }
        let target_source = fs::canonicalize(target_path).map_err(|source| PolicyError::Read {
            path: target_path.to_path_buf(),
            source,
        })?;
        if !target_source.is_file() || !target_source.starts_with(package_root) {
            return Err(PolicyError::InvalidUnsafeBoundary {
                detail: format!(
                    "Cargo target escapes its package: {}",
                    target_path.display()
                ),
            });
        }
        let reachable = reachable_module_map(package_root, &target_source, target.name.as_str())?;
        if reachable
            .get(module)
            .is_some_and(|declared| declared == source)
        {
            return Ok(Some(LibraryModuleGraph {
                target_source,
                modules: reachable,
            }));
        }
    }
    Ok(None)
}

fn reachable_module_map(
    package_root: &Path,
    target_source: &Path,
    target_module: &str,
) -> Result<BTreeMap<String, PathBuf>, PolicyError> {
    let mut reachable = BTreeMap::new();
    let mut source_identities = BTreeMap::new();
    let mut pending = vec![(target_module.to_owned(), target_source.to_path_buf())];
    while let Some((module_identity, source)) = pending.pop() {
        if let Some(existing) = reachable.get(&module_identity) {
            if existing != &source {
                return Err(PolicyError::InvalidUnsafeBoundary {
                    detail: format!(
                        "Rust module {module_identity} resolves to multiple compiler inputs"
                    ),
                });
            }
            continue;
        }
        if let Some(existing) = source_identities.insert(source.clone(), module_identity.clone())
            && existing != module_identity
        {
            return Err(PolicyError::InvalidUnsafeBoundary {
                detail: format!(
                    "compiler input {} has multiple module identities: {existing} and {module_identity}",
                    source.display()
                ),
            });
        }
        reachable.insert(module_identity.clone(), source.clone());
        let text = fs::read_to_string(&source).map_err(|error| PolicyError::Read {
            path: source.clone(),
            source: error,
        })?;
        let file = syn::parse_file(&text).map_err(|error| PolicyError::InvalidUnsafeBoundary {
            detail: format!("cannot parse Rust module {}: {error}", source.display()),
        })?;
        let module_root = module_child_root(&source)?;
        let source_directory =
            source
                .parent()
                .ok_or_else(|| PolicyError::InvalidUnsafeBoundary {
                    detail: format!("Rust module has no parent: {}", source.display()),
                })?;
        let mut declared = Vec::new();
        collect_external_modules(
            &file.items,
            &module_root,
            source_directory,
            &module_identity,
            &mut declared,
        )?;
        for (declared_identity, candidates) in declared {
            let existing = candidates
                .into_iter()
                .filter(|candidate| candidate.exists())
                .collect::<Vec<_>>();
            let [requested] = existing.as_slice() else {
                if existing.is_empty() {
                    continue;
                }
                return Err(PolicyError::InvalidUnsafeBoundary {
                    detail: format!(
                        "Rust module {declared_identity} has ambiguous compiler inputs"
                    ),
                });
            };
            pending.push((
                declared_identity,
                canonical_compiler_input(package_root, requested, "declared Rust module")?,
            ));
        }
    }
    Ok(reachable)
}

fn module_child_root(source: &Path) -> Result<PathBuf, PolicyError> {
    let parent = source
        .parent()
        .ok_or_else(|| PolicyError::InvalidUnsafeBoundary {
            detail: format!("Rust module has no parent: {}", source.display()),
        })?;
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| PolicyError::InvalidUnsafeBoundary {
            detail: format!("Rust module has no Unicode stem: {}", source.display()),
        })?;
    Ok(if matches!(stem, "lib" | "main" | "mod") {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    })
}

fn collect_external_modules(
    items: &[Item],
    module_root: &Path,
    source_directory: &Path,
    parent_identity: &str,
    declared: &mut Vec<(String, Vec<PathBuf>)>,
) -> Result<(), PolicyError> {
    for item in items {
        let Item::Mod(module) = item else {
            continue;
        };
        let module_name = module.ident.to_string();
        let module_identity = format!("{parent_identity}::{module_name}");
        if let Some((_, nested)) = &module.content {
            collect_external_modules(
                nested,
                &module_root.join(module_name),
                source_directory,
                &module_identity,
                declared,
            )?;
            continue;
        }
        if let Some(explicit_path) = module_path_attribute(&module.attrs)? {
            declared.push((module_identity, vec![source_directory.join(explicit_path)]));
        } else {
            declared.push((
                module_identity,
                vec![
                    module_root.join(format!("{module_name}.rs")),
                    module_root.join(module_name).join("mod.rs"),
                ],
            ));
        }
    }
    Ok(())
}

fn module_path_attribute(attributes: &[Attribute]) -> Result<Option<PathBuf>, PolicyError> {
    if attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg_attr")
            && attribute_tokens_contain_identifier(attribute, "path")
    }) {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: "conditional #[cfg_attr(path = ...)] compiler inputs are not allowed"
                .to_owned(),
        });
    }
    let mut paths = attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("path"));
    let Some(attribute) = paths.next() else {
        return Ok(None);
    };
    if paths.next().is_some() {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: "Rust module declares more than one #[path] compiler input".to_owned(),
        });
    }
    let Meta::NameValue(name_value) = &attribute.meta else {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: "Rust module #[path] must be a literal name-value attribute".to_owned(),
        });
    };
    let Expr::Lit(expression) = &name_value.value else {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: "Rust module #[path] must not use a generated input".to_owned(),
        });
    };
    let Lit::Str(path) = &expression.lit else {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: "Rust module #[path] must contain a string literal".to_owned(),
        });
    };
    Ok(Some(PathBuf::from(path.value())))
}

fn validate_unsafe_boundary_governance(
    package: &Package,
    package_root: &Path,
    source: &Path,
    adr: &Path,
    boundary: &UnsafeBoundary,
) -> Result<(), PolicyError> {
    let manifest_path = package.manifest_path.as_std_path();
    let manifest_text = fs::read_to_string(manifest_path).map_err(|source| PolicyError::Read {
        path: manifest_path.to_path_buf(),
        source,
    })?;
    let manifest: toml::Value =
        toml::from_str(&manifest_text).map_err(|source| PolicyError::Parse {
            path: manifest_path.to_path_buf(),
            source,
        })?;
    let adr_text = fs::read_to_string(adr).map_err(|source| PolicyError::Read {
        path: adr.to_path_buf(),
        source,
    })?;
    validate_adr_header(adr, &adr_text, boundary)?;

    let module_graph =
        package_target_defining_module(package, package_root, boundary.module.as_str(), source)?
            .ok_or_else(|| PolicyError::InvalidUnsafeBoundary {
                detail: format!(
                    "{} is not declared as module {} from a library target in {}",
                    boundary.source.display(),
                    boundary.module,
                    package.name
                ),
            })?;
    if !module_graph.target_source.starts_with(package_root) {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: format!("package {} target source escapes its package", package.name),
        });
    }
    let lint_inventory = module_graph_unsafe_lint_inventory(&module_graph)?;
    let lints = manifest.get("lints");
    if !boundary_lint_state_is_valid(
        boundary.status,
        lints,
        lint_inventory.as_ref(),
        boundary.expected_source_tokens,
        boundary.expected_geiger_count,
    ) {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: format!(
                "{} boundary {} must retain workspace forbid, one target-root forbid declaration, no reachable override, and zero inventory",
                match boundary.status {
                    UnsafeBoundaryStatus::Proposed => "proposed",
                    UnsafeBoundaryStatus::Accepted => "accepted",
                },
                boundary.source.display()
            ),
        });
    }
    Ok(())
}

fn validate_adr_header(
    adr: &Path,
    text: &str,
    boundary: &UnsafeBoundary,
) -> Result<(), PolicyError> {
    let (identifier, fields) =
        parse_adr_header(adr, text).ok_or_else(|| PolicyError::InvalidUnsafeBoundary {
            detail: format!(
                "{} must have one strict metadata header before Context",
                boundary.adr.display()
            ),
        })?;
    let expected_fields = BTreeSet::from([
        "Decision date",
        "Manifest",
        "Module",
        "Owner",
        "Package",
        "Proposal date",
        "Related baseline",
        "Source",
        "Status",
    ]);
    if fields.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected_fields {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: format!(
                "{} has missing or unknown ADR metadata fields",
                boundary.adr.display()
            ),
        });
    }
    let expected_status = match boundary.status {
        UnsafeBoundaryStatus::Proposed => "Proposed",
        UnsafeBoundaryStatus::Accepted => "Accepted",
    };
    let package_identity = format!("{}@{}", boundary.package, boundary.package_version);
    let proposal_date = fields.get("Proposal date").map(String::as_str);
    let decision_date = fields.get("Decision date").map(String::as_str);
    let identity_matches = fields.get("Status").map(String::as_str) == Some(expected_status)
        && fields.get("Owner").map(String::as_str) == Some(boundary.owner.as_str())
        && fields.get("Package").map(String::as_str) == Some(package_identity.as_str())
        && fields.get("Manifest").map(String::as_str) == boundary.manifest.to_str()
        && fields.get("Module").map(String::as_str) == Some(boundary.module.as_str())
        && fields.get("Source").map(String::as_str) == boundary.source.to_str();
    if !identity_matches
        || proposal_date.and_then(parse_iso_date).is_none()
        || !adr_decision_state_is_valid(boundary.status, proposal_date, decision_date)
        || fields
            .get("Related baseline")
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: format!(
                "{} metadata does not bind {identifier} to the exact boundary identity and state",
                boundary.adr.display()
            ),
        });
    }
    Ok(())
}

fn parse_adr_header(adr: &Path, text: &str) -> Option<(String, BTreeMap<String, String>)> {
    let filename = adr.file_name()?.to_str()?;
    let mut filename_parts = filename.splitn(3, '-');
    if filename_parts.next()? != "ADR" {
        return None;
    }
    let number = filename_parts.next()?;
    if number.len() != 3 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let identifier = format!("ADR-{number}");
    let mut lines = text.lines();
    let title = lines.next()?;
    let title_prefix = format!("# {identifier}: ");
    if !title.starts_with(&title_prefix) || title.len() == title_prefix.len() {
        return None;
    }
    if !lines.next()?.is_empty() {
        return None;
    }
    let mut fields = BTreeMap::new();
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        let metadata = line.strip_prefix("**")?;
        let (key, value) = metadata.split_once(":** ")?;
        if key.is_empty()
            || value.is_empty()
            || fields.insert(key.to_owned(), value.to_owned()).is_some()
        {
            return None;
        }
    }
    if lines.next()? != "## Context" {
        return None;
    }
    Some((identifier, fields))
}

fn adr_decision_state_is_valid(
    status: UnsafeBoundaryStatus,
    proposal_date: Option<&str>,
    decision_date: Option<&str>,
) -> bool {
    let Some(proposal_date) = proposal_date.and_then(parse_iso_date) else {
        return false;
    };
    match status {
        UnsafeBoundaryStatus::Proposed => decision_date == Some("not accepted"),
        UnsafeBoundaryStatus::Accepted => decision_date
            .and_then(parse_iso_date)
            .is_some_and(|decision_date| decision_date >= proposal_date),
    }
}

fn parse_iso_date(value: &str) -> Option<(u16, u8, u8)> {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return None;
    }
    let year = value.get(..4)?.parse::<u16>().ok()?;
    let month = value.get(5..7)?.parse::<u8>().ok()?;
    let day = value.get(8..)?.parse::<u8>().ok()?;
    if year == 0 || !(1..=12).contains(&month) {
        return None;
    }
    let leap_year =
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let maximum_day = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=maximum_day)
        .contains(&day)
        .then_some((year, month, day))
}

fn boundary_lint_state_is_valid(
    status: UnsafeBoundaryStatus,
    lints: Option<&toml::Value>,
    lint_inventory: Option<&UnsafeLintInventory>,
    expected_source_tokens: usize,
    expected_geiger_count: usize,
) -> bool {
    match status {
        UnsafeBoundaryStatus::Proposed => {
            let inherits_workspace = lints
                .and_then(|value| value.get("workspace"))
                .and_then(toml::Value::as_bool)
                == Some(true);
            inherits_workspace
                && lint_inventory.is_some_and(|inventory| {
                    inventory.target
                        == vec![UnsafeLintDeclaration {
                            level: UnsafeLintLevel::Forbid,
                            is_inner: true,
                        }]
                        && inventory.reachable_non_target.is_empty()
                })
                && expected_source_tokens == 0
                && expected_geiger_count == 0
        }
        UnsafeBoundaryStatus::Accepted => false,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct UnsafeLintInventory {
    target: Vec<UnsafeLintDeclaration>,
    reachable_non_target: Vec<UnsafeLintDeclaration>,
}

fn module_graph_unsafe_lint_inventory(
    graph: &LibraryModuleGraph,
) -> Result<Option<UnsafeLintInventory>, PolicyError> {
    let mut target = None;
    let mut reachable_non_target = Vec::new();
    for source in graph.modules.values() {
        let text = fs::read_to_string(source).map_err(|error| PolicyError::Read {
            path: source.clone(),
            source: error,
        })?;
        let Some(mut declarations) = unsafe_lint_declarations(&text) else {
            return Ok(None);
        };
        if source == &graph.target_source {
            target = Some(declarations);
        } else {
            reachable_non_target.append(&mut declarations);
        }
    }
    Ok(target.map(|target| UnsafeLintInventory {
        target,
        reachable_non_target,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnsafeLintLevel {
    Allow,
    Warn,
    Expect,
    Deny,
    Forbid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnsafeLintDeclaration {
    level: UnsafeLintLevel,
    is_inner: bool,
}

#[derive(Default)]
struct UnsafeLintVisitor {
    declarations: Vec<UnsafeLintDeclaration>,
    has_conditional_declaration: bool,
    has_invalid_declaration: bool,
}

impl<'ast> Visit<'ast> for UnsafeLintVisitor {
    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        if attribute.path().is_ident("cfg_attr")
            && attribute_tokens_contain_identifier(attribute, "unsafe_code")
        {
            self.has_conditional_declaration = true;
        }
        for (name, level) in [
            ("allow", UnsafeLintLevel::Allow),
            ("warn", UnsafeLintLevel::Warn),
            ("expect", UnsafeLintLevel::Expect),
            ("deny", UnsafeLintLevel::Deny),
            ("forbid", UnsafeLintLevel::Forbid),
        ] {
            match attribute_has_unsafe_lint(attribute, name) {
                Ok(true) => {
                    self.declarations.push(UnsafeLintDeclaration {
                        level,
                        is_inner: matches!(attribute.style, AttrStyle::Inner(_)),
                    });
                }
                Ok(false) => {}
                Err(()) => self.has_invalid_declaration = true,
            }
        }
        visit::visit_attribute(self, attribute);
    }
}

fn unsafe_lint_declarations(text: &str) -> Option<Vec<UnsafeLintDeclaration>> {
    let file = syn::parse_file(text).ok()?;
    let mut visitor = UnsafeLintVisitor::default();
    visitor.visit_file(&file);
    (!visitor.has_conditional_declaration && !visitor.has_invalid_declaration)
        .then_some(visitor.declarations)
}

fn attribute_has_unsafe_lint(attribute: &Attribute, level: &str) -> Result<bool, ()> {
    if !attribute.path().is_ident(level) {
        return Ok(false);
    }
    let Meta::List(_) = &attribute.meta else {
        return Ok(false);
    };
    let mut has_unsafe_code = false;
    let mut has_reason = false;
    attribute
        .parse_nested_meta(|meta| {
            if meta.path.is_ident("reason") {
                if has_reason {
                    return Err(meta.error("lint attribute may contain only one reason"));
                }
                let value = meta.value()?;
                value.parse::<LitStr>()?;
                has_reason = true;
            } else {
                has_unsafe_code |= meta.path.is_ident("unsafe_code");
            }
            Ok(())
        })
        .map_err(|_| ())?;
    Ok(has_unsafe_code)
}

fn attribute_tokens_contain_identifier(attribute: &Attribute, expected: &str) -> bool {
    match &attribute.meta {
        Meta::List(arguments) => {
            rust_text_contains_identifier(&arguments.tokens.to_string(), expected)
        }
        Meta::Path(_) | Meta::NameValue(_) => false,
    }
}

fn validate_relative_policy_path(path: &Path) -> Result<(), PolicyError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .as_os_str()
            .to_str()
            .is_none_or(|value| value.contains('\\'))
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: format!("invalid relative path {}", path.display()),
        });
    }
    Ok(())
}

fn scan_rust_tree(
    root: &Path,
    observed: &mut BTreeMap<PathBuf, UnsafeObservation>,
) -> Result<(), PolicyError> {
    if !root.exists() {
        return Ok(());
    }
    let canonical_root = fs::canonicalize(root).map_err(|source| PolicyError::Read {
        path: root.to_path_buf(),
        source,
    })?;
    if !canonical_root.is_dir() {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: format!(
                "unsafe inventory root is not a directory: {}",
                root.display()
            ),
        });
    }

    let mut directories = vec![canonical_root.clone()];
    let mut pending_files = Vec::new();
    while let Some(directory) = directories.pop() {
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
            if path_is_link_or_reparse(&path)? {
                return Err(PolicyError::InvalidUnsafeBoundary {
                    detail: format!(
                        "unsafe inventory rejects symlink or reparse input {}",
                        path.display()
                    ),
                });
            }
            let canonical = fs::canonicalize(&path).map_err(|source| PolicyError::Read {
                path: path.clone(),
                source,
            })?;
            if canonical != path || !canonical.starts_with(&canonical_root) {
                return Err(PolicyError::InvalidUnsafeBoundary {
                    detail: format!(
                        "unsafe inventory input escapes or aliases {}",
                        path.display()
                    ),
                });
            }
            if file_type.is_dir() {
                directories.push(canonical);
            } else if file_type.is_file()
                && path.extension().and_then(|value| value.to_str()) == Some("rs")
            {
                pending_files.push(canonical);
            }
        }
    }

    let mut processed = BTreeSet::new();
    while let Some(path) = pending_files.pop() {
        if !processed.insert(path.clone()) {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|source| PolicyError::Read {
            path: path.clone(),
            source,
        })?;
        for input in rust_compiler_inputs(&path, &text)? {
            pending_files.push(canonical_compiler_input(
                &canonical_root,
                &input,
                "Rust compiler input",
            )?);
        }
        let observation = scan_rust_file(&path)?;
        if observed.insert(path.clone(), observation).is_some() {
            return Err(PolicyError::InvalidUnsafeBoundary {
                detail: format!(
                    "unsafe inventory scanned the same compiler input twice: {}",
                    path.display()
                ),
            });
        }
    }
    Ok(())
}

fn path_is_link_or_reparse(path: &Path) -> Result<bool, PolicyError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PolicyError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn canonical_compiler_input(
    root: &Path,
    requested: &Path,
    description: &str,
) -> Result<PathBuf, PolicyError> {
    let relative =
        requested
            .strip_prefix(root)
            .map_err(|_| PolicyError::InvalidUnsafeBoundary {
                detail: format!("{description} escapes {}", requested.display()),
            })?;
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: format!("{description} escapes or aliases {}", requested.display()),
        });
    }
    if path_is_link_or_reparse(requested)? {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: format!(
                "{description} is a symlink or reparse point: {}",
                requested.display()
            ),
        });
    }
    let canonical = fs::canonicalize(requested).map_err(|source| PolicyError::Read {
        path: requested.to_path_buf(),
        source,
    })?;
    if !canonical.is_file() || !canonical.starts_with(root) || canonical != requested {
        return Err(PolicyError::InvalidUnsafeBoundary {
            detail: format!("{description} escapes or aliases {}", requested.display()),
        });
    }
    Ok(canonical)
}

#[derive(Debug)]
enum CompilerInput {
    Literal(PathBuf),
    Generated(&'static str),
}

#[derive(Default)]
struct CompilerInputVisitor {
    inputs: Vec<CompilerInput>,
}

impl<'ast> Visit<'ast> for CompilerInputVisitor {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if macro_path_is_include(&node.path) {
            match node.parse_body::<LitStr>() {
                Ok(path) => self
                    .inputs
                    .push(CompilerInput::Literal(path.value().into())),
                Err(_) => self.inputs.push(CompilerInput::Generated("include!")),
            }
        } else {
            // This catches only obvious Proposed-state wrappers. Stable syn cannot
            // inventory expansion, so every Accepted boundary is rejected earlier.
            if rust_text_contains_identifier(&node.tokens.to_string(), "include") {
                self.inputs
                    .push(CompilerInput::Generated("macro-expanded include!"));
            }
        }
        visit::visit_macro(self, node);
    }

    fn visit_attribute(&mut self, node: &'ast Attribute) {
        if node.path().is_ident("cfg_attr") && attribute_tokens_contain_identifier(node, "path") {
            self.inputs
                .push(CompilerInput::Generated("#[cfg_attr(path = ...)]"));
        } else if node.path().is_ident("path") {
            match &node.meta {
                Meta::NameValue(name_value) => match &name_value.value {
                    Expr::Lit(expression) => match &expression.lit {
                        Lit::Str(path) => {
                            self.inputs
                                .push(CompilerInput::Literal(path.value().into()));
                        }
                        _ => self.inputs.push(CompilerInput::Generated("#[path]")),
                    },
                    _ => self.inputs.push(CompilerInput::Generated("#[path]")),
                },
                Meta::Path(_) | Meta::List(_) => {
                    self.inputs.push(CompilerInput::Generated("#[path]"));
                }
            }
        }
        visit::visit_attribute(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        if use_tree_contains_identifier(&node.tree, "include") {
            self.inputs
                .push(CompilerInput::Generated("aliased include!"));
        }
        visit::visit_item_use(self, node);
    }
}

fn macro_path_is_include(path: &SynPath) -> bool {
    path.segments
        .last()
        .is_some_and(|segment| segment.ident == "include")
}

fn use_tree_contains_identifier(tree: &UseTree, expected: &str) -> bool {
    match tree {
        UseTree::Path(path) => {
            path.ident == expected || use_tree_contains_identifier(&path.tree, expected)
        }
        UseTree::Name(name) => name.ident == expected,
        UseTree::Rename(rename) => rename.ident == expected || rename.rename == expected,
        UseTree::Group(group) => group
            .items
            .iter()
            .any(|item| use_tree_contains_identifier(item, expected)),
        UseTree::Glob(_) => false,
    }
}

fn rust_compiler_inputs(source: &Path, text: &str) -> Result<Vec<PathBuf>, PolicyError> {
    let file = syn::parse_file(text).map_err(|error| PolicyError::InvalidUnsafeBoundary {
        detail: format!(
            "unsafe inventory cannot parse compiler input {}: {error}",
            source.display()
        ),
    })?;
    let mut visitor = CompilerInputVisitor::default();
    visitor.visit_file(&file);
    let source_directory = source
        .parent()
        .ok_or_else(|| PolicyError::InvalidUnsafeBoundary {
            detail: format!("compiler input has no parent: {}", source.display()),
        })?;
    visitor
        .inputs
        .into_iter()
        .map(|input| match input {
            CompilerInput::Literal(relative) => {
                if relative.is_absolute() {
                    Err(PolicyError::InvalidUnsafeBoundary {
                        detail: format!(
                            "compiler input must be relative in {}: {}",
                            source.display(),
                            relative.display()
                        ),
                    })
                } else {
                    Ok(source_directory.join(relative))
                }
            }
            CompilerInput::Generated(kind) => Err(PolicyError::InvalidUnsafeBoundary {
                detail: format!(
                    "{kind} in {} must use a literal, scanned compiler input; generated or OUT_DIR inputs are not allowed",
                    source.display()
                ),
            }),
        })
        .collect()
}

fn rust_text_contains_identifier(text: &str, expected: &str) -> bool {
    let bytes = text.as_bytes();
    let expected = expected.as_bytes();
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
            if &bytes[start..index] == expected {
                return true;
            }
        } else {
            index += 1;
        }
    }
    false
}

fn scan_rust_file(path: &Path) -> Result<UnsafeObservation, PolicyError> {
    let text = fs::read_to_string(path).map_err(|source| PolicyError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut observation = UnsafeObservation::default();
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
                observation.count = observation.count.saturating_add(1);
                if observation.first_line == 0 {
                    observation.first_line = line_number(bytes, start);
                }
            }
        } else {
            index += 1;
        }
    }
    Ok(observation)
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

#[derive(Debug, Clone, Copy, Default)]
struct UnsafeObservation {
    count: usize,
    first_line: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnsafePolicy {
    schema_version: String,
    boundaries: Vec<UnsafeBoundary>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnsafeBoundary {
    package: String,
    package_version: String,
    manifest: PathBuf,
    module: String,
    source: PathBuf,
    status: UnsafeBoundaryStatus,
    adr: PathBuf,
    owner: String,
    reason: String,
    expected_source_tokens: usize,
    expected_geiger_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UnsafeBoundaryStatus {
    Proposed,
    Accepted,
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
    #[error("POLICY_UNSAFE_BOUNDARY: {detail}")]
    InvalidUnsafeBoundary { detail: String },
    #[error("POLICY_UNSAFE_BOUNDARY: duplicate boundary for {path}")]
    DuplicateUnsafeBoundary { path: PathBuf },
    #[error("POLICY_UNSAFE_COUNT: {path} expected {expected} unsafe tokens, observed {observed}")]
    UnsafeBoundaryCount {
        path: PathBuf,
        expected: usize,
        observed: usize,
    },
    #[error("POLICY_UNSAFE: unsafe token in {path}:{line}")]
    UnsafeToken { path: PathBuf, line: usize },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn lint_inventory(target: &str, reachable_non_target: &[&str]) -> Option<UnsafeLintInventory> {
        let target = unsafe_lint_declarations(target)?;
        let mut reachable = Vec::new();
        for source in reachable_non_target {
            reachable.extend(unsafe_lint_declarations(source)?);
        }
        Some(UnsafeLintInventory {
            target,
            reachable_non_target: reachable,
        })
    }

    fn fixture_boundary(status: UnsafeBoundaryStatus) -> UnsafeBoundary {
        UnsafeBoundary {
            package: "rootlight-vfs".to_owned(),
            package_version: "0.1.0".to_owned(),
            manifest: "crates/rootlight-vfs/Cargo.toml".into(),
            module: "rootlight_vfs::platform::os".to_owned(),
            source: "crates/rootlight-vfs/src/platform/os.rs".into(),
            status,
            adr: "policy/adr/ADR-026-fixture.md".into(),
            owner: "@tomasmarekk".to_owned(),
            reason: "fixture".to_owned(),
            expected_source_tokens: usize::from(status == UnsafeBoundaryStatus::Accepted),
            expected_geiger_count: usize::from(status == UnsafeBoundaryStatus::Accepted),
        }
    }

    #[test]
    fn unsafe_boundary_module_uses_the_declared_module_identity() {
        let directory = tempdir().expect("temporary directory is available");
        let package = directory.path().join("rootlight-vfs");
        let module = package.join("src");
        fs::create_dir_all(&module).expect("module directory creates");
        let target = module.join("lib.rs");
        let source = module.join("physical.rs");
        fs::write(&target, "#[path = \"physical.rs\"]\nmod logical;\n")
            .expect("target source writes");
        fs::write(&source, "").expect("module source writes");
        let package = fs::canonicalize(package).expect("package path canonicalizes");
        let target = fs::canonicalize(target).expect("target path canonicalizes");
        let source = fs::canonicalize(source).expect("source path canonicalizes");
        let modules = reachable_module_map(&package, &target, "rootlight_vfs")
            .expect("module graph resolves");

        assert_eq!(modules.get("rootlight_vfs::logical"), Some(&source));
        assert!(!modules.contains_key("rootlight_vfs::physical"));
    }

    #[test]
    fn cargo_library_target_name_defines_the_crate_module_identity() {
        let directory = tempdir().expect("temporary directory is available");
        let package_root = directory.path().join("hyphenated-package");
        let source_root = package_root.join("src");
        fs::create_dir_all(&source_root).expect("source directory creates");
        fs::write(
            package_root.join("Cargo.toml"),
            "[package]\nname = \"hyphenated-package\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\nname = \"custom_crate\"\n",
        )
        .expect("manifest writes");
        fs::write(source_root.join("lib.rs"), "mod logical;\n").expect("target source writes");
        fs::write(source_root.join("logical.rs"), "").expect("module source writes");
        let metadata = MetadataCommand::new()
            .manifest_path(package_root.join("Cargo.toml"))
            .no_deps()
            .exec()
            .expect("Cargo metadata resolves");
        let package = metadata.root_package().expect("root package exists");
        let package_root = fs::canonicalize(package_root).expect("package root canonicalizes");
        let source =
            fs::canonicalize(source_root.join("logical.rs")).expect("source canonicalizes");

        let graph = package_target_defining_module(
            package,
            &package_root,
            "custom_crate::logical",
            &source,
        )
        .expect("module identity resolves")
        .expect("custom target declares module");

        assert_eq!(graph.modules.get("custom_crate::logical"), Some(&source));
        assert!(
            package_target_defining_module(
                package,
                &package_root,
                "hyphenated-package::logical",
                &source
            )
            .expect("package-name mismatch is checked")
            .is_none()
        );
    }

    #[test]
    fn proposed_boundaries_require_exact_reachable_lint_state() {
        let proposed_manifest: toml::Value =
            toml::from_str("[lints]\nworkspace = true\n").expect("fixture parses");
        let accepted_manifest: toml::Value =
            toml::from_str("[lints.rust]\nunsafe_code = \"deny\"\n").expect("fixture parses");
        let exact = lint_inventory(
            "#![forbid(unsafe_code, reason = \"workspace safety baseline\")]",
            &["fn safe() {}"],
        )
        .expect("exact lint state parses");

        assert!(boundary_lint_state_is_valid(
            UnsafeBoundaryStatus::Proposed,
            proposed_manifest.get("lints"),
            Some(&exact),
            0,
            0,
        ));
        assert!(!boundary_lint_state_is_valid(
            UnsafeBoundaryStatus::Proposed,
            accepted_manifest.get("lints"),
            Some(&exact),
            0,
            0,
        ));
        assert!(!boundary_lint_state_is_valid(
            UnsafeBoundaryStatus::Accepted,
            accepted_manifest.get("lints"),
            Some(&exact),
            2,
            1,
        ));
        assert!(!boundary_lint_state_is_valid(
            UnsafeBoundaryStatus::Proposed,
            proposed_manifest.get("lints"),
            lint_inventory(
                "// #![forbid(unsafe_code)]\nconst CLAIM: &str = \"#![forbid(unsafe_code)]\";",
                &[]
            )
            .as_ref(),
            0,
            0,
        ));
        for target in [
            "#![forbid(unsafe_code)]\n#![allow(unsafe_code)]",
            "#![forbid(unsafe_code)]\n#![warn(unsafe_code)]",
            "#![forbid(unsafe_code)]\n#![expect(unsafe_code)]",
            "#![forbid(unsafe_code)]\n#![allow(unsafe_code, reason = \"late override\")]",
            "#![cfg_attr(any(), forbid(unsafe_code))]",
        ] {
            let inventory = lint_inventory(target, &[]);
            assert!(!boundary_lint_state_is_valid(
                UnsafeBoundaryStatus::Proposed,
                proposed_manifest.get("lints"),
                inventory.as_ref(),
                0,
                0,
            ));
        }
        let reachable_override = lint_inventory(
            "#![forbid(unsafe_code)]",
            &[
                "fn safe() {}",
                "#![allow(unsafe_code, reason = \"reachable sibling override\")]\nfn hidden() {}",
            ],
        );
        assert!(!boundary_lint_state_is_valid(
            UnsafeBoundaryStatus::Proposed,
            proposed_manifest.get("lints"),
            reachable_override.as_ref(),
            0,
            0,
        ));
        assert!(
            unsafe_lint_declarations("#![forbid(unsafe_code, reason = \"one\", reason = \"two\")]")
                .is_none()
        );
    }

    #[test]
    fn reachable_module_lint_inventory_rejects_a_sibling_override() {
        let directory = tempdir().expect("temporary directory is available");
        let source_root = directory.path().join("src");
        fs::create_dir(&source_root).expect("source directory creates");
        fs::write(
            source_root.join("lib.rs"),
            "#![forbid(unsafe_code, reason = \"workspace safety baseline\")]\nmod boundary;\nmod sibling;\n",
        )
        .expect("target source writes");
        fs::write(source_root.join("boundary.rs"), "fn safe() {}\n")
            .expect("boundary source writes");
        fs::write(
            source_root.join("sibling.rs"),
            "#![allow(unsafe_code, reason = \"late reachable override\")]\nfn hidden() {}\n",
        )
        .expect("sibling source writes");
        let package_root = fs::canonicalize(directory.path()).expect("package root canonicalizes");
        let target_source =
            fs::canonicalize(source_root.join("lib.rs")).expect("target source canonicalizes");
        let modules = reachable_module_map(&package_root, &target_source, "fixture_crate")
            .expect("module graph resolves");
        let graph = LibraryModuleGraph {
            target_source,
            modules,
        };
        let inventory = module_graph_unsafe_lint_inventory(&graph)
            .expect("lint inventory reads")
            .expect("lint declarations parse");
        let manifest: toml::Value =
            toml::from_str("[lints]\nworkspace = true\n").expect("fixture parses");

        assert_eq!(
            inventory.reachable_non_target,
            vec![UnsafeLintDeclaration {
                level: UnsafeLintLevel::Allow,
                is_inner: true,
            }]
        );
        assert!(!boundary_lint_state_is_valid(
            UnsafeBoundaryStatus::Proposed,
            manifest.get("lints"),
            Some(&inventory),
            0,
            0,
        ));
    }

    #[test]
    fn unsafe_boundary_status_requires_a_consistent_decision_date() {
        assert!(adr_decision_state_is_valid(
            UnsafeBoundaryStatus::Proposed,
            Some("2026-07-17"),
            Some("not accepted")
        ));
        assert!(!adr_decision_state_is_valid(
            UnsafeBoundaryStatus::Proposed,
            Some("2026-02-30"),
            Some("2026-07-17")
        ));
        assert!(adr_decision_state_is_valid(
            UnsafeBoundaryStatus::Accepted,
            Some("2026-07-17"),
            Some("2026-07-17")
        ));
        assert!(!adr_decision_state_is_valid(
            UnsafeBoundaryStatus::Accepted,
            Some("2026-07-18"),
            Some("2026-07-17")
        ));
        assert!(!adr_decision_state_is_valid(
            UnsafeBoundaryStatus::Accepted,
            Some("2026-07-17"),
            Some("not accepted")
        ));
    }

    #[test]
    fn unsafe_boundary_adr_header_binds_identity_outside_fences() {
        let boundary = fixture_boundary(UnsafeBoundaryStatus::Proposed);
        let header = "# ADR-026: Fixture\n\n**Status:** Proposed\n**Owner:** @tomasmarekk\n**Proposal date:** 2026-07-17\n**Decision date:** not accepted\n**Package:** rootlight-vfs@0.1.0\n**Manifest:** crates/rootlight-vfs/Cargo.toml\n**Module:** rootlight_vfs::platform::os\n**Source:** crates/rootlight-vfs/src/platform/os.rs\n**Related baseline:** ADR-010\n\n## Context\n";
        let path = Path::new("policy/adr/ADR-026-fixture.md");

        validate_adr_header(path, header, &boundary).expect("exact ADR header passes");
        assert!(
            validate_adr_header(
                path,
                &header.replace("@tomasmarekk", "@substitute"),
                &boundary
            )
            .is_err()
        );
        assert!(
            validate_adr_header(
                path,
                "# ADR-026: Fixture\n\n```\n**Status:** Proposed\n```\n\n## Context\n",
                &boundary
            )
            .is_err()
        );
    }

    #[test]
    fn unsafe_scan_inventories_literal_include_inputs() {
        let directory = tempdir().expect("temporary directory is available");
        let source = directory.path().join("lib.rs");
        let included = directory.path().join("hidden.compiler-input");
        fs::write(&source, "include!(\"hidden.compiler-input\");\n")
            .expect("source fixture writes");
        fs::write(
            &included,
            "fn fixture() { unsafe { core::hint::unreachable_unchecked() } }\n",
        )
        .expect("included fixture writes");
        let mut observed = BTreeMap::new();

        scan_rust_tree(directory.path(), &mut observed).expect("compiler inventory succeeds");

        let included = fs::canonicalize(included).expect("included input canonicalizes");
        assert_eq!(observed.get(&included).map(|item| item.count), Some(1));
    }

    #[test]
    fn accepted_boundary_rejects_unexpanded_macro_generated_code() {
        let directory = tempdir().expect("temporary directory is available");
        let source = directory.path().join("lib.rs");
        let text = "emit_generated!();\n";
        fs::write(&source, text).expect("source fixture writes");

        assert!(
            rust_compiler_inputs(&source, text)
                .expect("syntactic scan cannot see expansion")
                .is_empty()
        );
        assert_eq!(
            scan_rust_file(&source)
                .expect("syntactic unsafe scan succeeds")
                .count,
            0
        );
        let policy = UnsafePolicy {
            schema_version: CURRENT_SCHEMA_VERSION.to_owned(),
            boundaries: vec![fixture_boundary(UnsafeBoundaryStatus::Accepted)],
        };
        let error = reject_accepted_boundaries_without_authoritative_evidence(&policy)
            .expect_err("Accepted must fail without compiler-derived evidence");

        assert_eq!(
            error.to_string(),
            format!("POLICY_UNSAFE_BOUNDARY: {ACCEPTED_UNSAFE_EVIDENCE_UNIMPLEMENTED}")
        );
    }

    #[test]
    fn unsafe_scan_rejects_generated_and_escaping_compiler_inputs() {
        let directory = tempdir().expect("temporary directory is available");
        let source = directory.path().join("lib.rs");
        fs::write(&source, "fn safe() {}\n").expect("source fixture writes");
        assert!(matches!(
            rust_compiler_inputs(
                &source,
                "include!(concat!(env!(\"OUT_DIR\"), \"/hidden.rs\"));\n"
            ),
            Err(PolicyError::InvalidUnsafeBoundary { .. })
        ));
        for bypass in [
            "#[cfg_attr(windows, path = \"hidden.rs\")]\nmod hidden;\n",
            "macro_rules! hidden { () => { include!(\"hidden.rs\"); } }\nhidden!();\n",
            "macro_rules! invoke { ($loader:ident) => { $loader!(\"hidden.rs\"); } }\ninvoke!(include);\n",
            "use crate::include as hidden;\nhidden!(\"hidden.rs\");\n",
        ] {
            assert!(matches!(
                rust_compiler_inputs(&source, bypass),
                Err(PolicyError::InvalidUnsafeBoundary { .. })
            ));
        }
        assert!(matches!(
            rust_compiler_inputs(
                &source,
                "#[path = concat!(\"hidden\", \".rs\")]\nmod hidden;\n"
            ),
            Err(PolicyError::InvalidUnsafeBoundary { .. })
        ));

        let root = fs::canonicalize(directory.path()).expect("root canonicalizes");
        let outside = tempdir().expect("outside directory is available");
        let escaping = outside.path().join("outside.rs");
        fs::write(&escaping, "unsafe fn hidden() {}\n").expect("outside fixture writes");
        let escaping = fs::canonicalize(escaping).expect("outside input canonicalizes");
        assert!(canonical_compiler_input(&root, &escaping, "fixture").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_scan_rejects_symlinked_compiler_inputs() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary directory is available");
        let outside = tempdir().expect("outside directory is available");
        fs::write(outside.path().join("unsafe.rs"), "unsafe fn hidden() {}\n")
            .expect("outside fixture writes");
        symlink(
            outside.path().join("unsafe.rs"),
            directory.path().join("linked.rs"),
        )
        .expect("symlink fixture creates");
        let mut observed = BTreeMap::new();

        assert!(matches!(
            scan_rust_tree(directory.path(), &mut observed),
            Err(PolicyError::InvalidUnsafeBoundary { .. })
        ));
    }

    #[test]
    fn unsafe_scan_ignores_comments_and_literals() {
        let directory = tempdir().expect("temporary directory is available");
        let source = directory.path().join("safe.rs");
        fs::write(
            &source,
            "// unsafe block\nconst LABEL: &str = \"unsafe\";\nconst RAW: &str = r#\"unsafe { ignored }\"#;\nconst BYTES: &[u8] = br\"unsafe\";\nconst C_BYTES: &CStr = cr#\"unsafe\"#;\nfn safe() {}\n",
        )
        .expect("fixture writes");

        assert_eq!(scan_rust_file(&source).expect("safe source scans").count, 0);
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

        let observation = scan_rust_file(&source).expect("source inventory succeeds");
        assert_eq!(observation.count, 1);
        assert_eq!(observation.first_line, 1);
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

        let observation = scan_rust_file(&source).expect("source inventory succeeds");
        assert_eq!(observation.count, 1);
        assert_eq!(observation.first_line, 1);
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

        let observation = scan_rust_file(&source).expect("source inventory succeeds");
        assert_eq!(observation.count, 1);
        assert_eq!(observation.first_line, 1);
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
