//! Enforces the checked Cargo workspace members and internal dependency graph.
//!
//! The allow-list is intentionally explicit: adding a crate or dependency edge
//! requires a reviewed policy change instead of silently widening architecture.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, Package, TargetKind};
use serde::Deserialize;

const POLICY_PATH: &str = "policy/architecture.toml";

pub(crate) fn check(fixture_root: Option<&Path>) -> Result<(), ArchitectureError> {
    let (metadata, policy_path) = load_metadata_and_policy(fixture_root)?;
    let policy_text =
        fs::read_to_string(&policy_path).map_err(|source| ArchitectureError::Read {
            path: policy_path.clone(),
            source,
        })?;
    let policy: ArchitecturePolicy =
        toml::from_str(&policy_text).map_err(|source| ArchitectureError::Parse {
            path: policy_path,
            source,
        })?;

    if policy.schema_version != "1.0" {
        return Err(ArchitectureError::UnsupportedPolicyVersion(
            policy.schema_version,
        ));
    }

    validate(&metadata, &policy)?;
    println!(
        "architecture check passed for {} workspace members",
        policy.workspace_members.len()
    );
    Ok(())
}

fn load_metadata_and_policy(
    fixture_root: Option<&Path>,
) -> Result<(Metadata, PathBuf), ArchitectureError> {
    match fixture_root {
        Some(root) => {
            let canonical_root = root
                .canonicalize()
                .map_err(|source| ArchitectureError::Read {
                    path: root.to_path_buf(),
                    source,
                })?;
            let manifest_path = canonical_root.join("Cargo.toml");
            let metadata = MetadataCommand::new()
                .manifest_path(&manifest_path)
                .exec()
                .map_err(ArchitectureError::Metadata)?;
            Ok((metadata, canonical_root.join(POLICY_PATH)))
        }
        None => {
            let metadata = MetadataCommand::new()
                .exec()
                .map_err(ArchitectureError::Metadata)?;
            let policy_path = metadata.workspace_root.as_std_path().join(POLICY_PATH);
            Ok((metadata, policy_path))
        }
    }
}

fn validate(metadata: &Metadata, policy: &ArchitecturePolicy) -> Result<(), ArchitectureError> {
    let workspace_packages: BTreeMap<&str, &Package> = metadata
        .workspace_packages()
        .into_iter()
        .map(|package| (package.name.as_ref(), package))
        .collect();
    let observed_members: BTreeSet<&str> = workspace_packages.keys().copied().collect();
    let expected_members: BTreeSet<&str> = policy
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();

    if observed_members != expected_members {
        return Err(ArchitectureError::MemberMismatch {
            expected: expected_members.into_iter().map(str::to_owned).collect(),
            observed: observed_members.into_iter().map(str::to_owned).collect(),
        });
    }

    let declared_crates: BTreeMap<&str, &CratePolicy> = policy
        .crates
        .iter()
        .map(|crate_policy| (crate_policy.name.as_str(), crate_policy))
        .collect();
    if declared_crates.keys().copied().collect::<BTreeSet<_>>() != expected_members {
        return Err(ArchitectureError::PolicyMemberMismatch);
    }

    let dev_only: BTreeSet<&str> = policy.dev_only_members.iter().map(String::as_str).collect();

    for (package_name, package) in &workspace_packages {
        let crate_policy = declared_crates
            .get(package_name)
            .ok_or(ArchitectureError::PolicyMemberMismatch)?;
        validate_package(package, crate_policy, &expected_members, &dev_only)?;
    }

    ensure_acyclic(&workspace_packages, &expected_members)?;
    Ok(())
}

fn validate_package(
    package: &Package,
    policy: &CratePolicy,
    workspace_members: &BTreeSet<&str>,
    dev_only: &BTreeSet<&str>,
) -> Result<(), ArchitectureError> {
    let allowed_internal: BTreeSet<&str> = policy
        .allowed_internal_dependencies
        .iter()
        .map(String::as_str)
        .collect();

    for dependency in &package.dependencies {
        let dependency_name = dependency.rename.as_deref().unwrap_or(&dependency.name);
        let Some(canonical_name) = workspace_members
            .iter()
            .find(|member| **member == dependency.name || **member == dependency_name)
            .copied()
        else {
            continue;
        };

        if dependency.kind == DependencyKind::Normal && !allowed_internal.contains(canonical_name) {
            return Err(ArchitectureError::ForbiddenEdge {
                from: package.name.to_string(),
                to: canonical_name.to_owned(),
            });
        }
        if dependency.kind == DependencyKind::Normal
            && !dev_only.contains(package.name.as_ref())
            && dev_only.contains(canonical_name)
        {
            return Err(ArchitectureError::ShippingDependsOnDevTool {
                from: package.name.to_string(),
                to: canonical_name.to_owned(),
            });
        }
    }

    let has_build_script = package
        .targets
        .iter()
        .any(|target| target.kind.contains(&TargetKind::CustomBuild));
    if has_build_script && !policy.allow_build_script {
        return Err(ArchitectureError::UnapprovedBuildScript(
            package.name.to_string(),
        ));
    }

    Ok(())
}

fn ensure_acyclic(
    packages: &BTreeMap<&str, &Package>,
    workspace_members: &BTreeSet<&str>,
) -> Result<(), ArchitectureError> {
    let mut temporary = BTreeSet::new();
    let mut permanent = BTreeSet::new();
    let mut stack = Vec::new();

    for package in packages.keys().copied() {
        visit(
            package,
            packages,
            workspace_members,
            &mut temporary,
            &mut permanent,
            &mut stack,
        )?;
    }
    Ok(())
}

fn visit<'a>(
    package: &'a str,
    packages: &BTreeMap<&'a str, &'a Package>,
    workspace_members: &BTreeSet<&'a str>,
    temporary: &mut BTreeSet<&'a str>,
    permanent: &mut BTreeSet<&'a str>,
    stack: &mut Vec<&'a str>,
) -> Result<(), ArchitectureError> {
    if permanent.contains(package) {
        return Ok(());
    }
    if !temporary.insert(package) {
        stack.push(package);
        return Err(ArchitectureError::Cycle(
            stack.iter().copied().map(str::to_owned).collect(),
        ));
    }

    stack.push(package);
    let package_metadata = packages
        .get(package)
        .ok_or_else(|| ArchitectureError::UnknownWorkspacePackage(package.to_owned()))?;
    for dependency in &package_metadata.dependencies {
        if dependency.kind != DependencyKind::Normal {
            continue;
        }
        if let Some(member) = workspace_members
            .iter()
            .find(|member| **member == dependency.name)
            .copied()
        {
            visit(
                member,
                packages,
                workspace_members,
                temporary,
                permanent,
                stack,
            )?;
        }
    }
    stack.pop();
    temporary.remove(package);
    permanent.insert(package);
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchitecturePolicy {
    schema_version: String,
    workspace_members: Vec<String>,
    dev_only_members: Vec<String>,
    crates: Vec<CratePolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CratePolicy {
    name: String,
    allowed_internal_dependencies: Vec<String>,
    allow_build_script: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ArchitectureError {
    #[error("failed to read architecture input at {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse architecture policy at {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to read Cargo metadata")]
    Metadata(#[source] cargo_metadata::Error),
    #[error("ARCH_POLICY_VERSION: unsupported architecture policy version {0}")]
    UnsupportedPolicyVersion(String),
    #[error("ARCH_MEMBER_MISMATCH: expected {expected:?}, observed {observed:?}")]
    MemberMismatch {
        expected: Vec<String>,
        observed: Vec<String>,
    },
    #[error("ARCH_POLICY_MEMBERS: workspace and crate policy members differ")]
    PolicyMemberMismatch,
    #[error("ARCH_FORBIDDEN_EDGE: {from} must not depend on {to}")]
    ForbiddenEdge { from: String, to: String },
    #[error("ARCH_DEV_EDGE: shipping crate {from} must not depend on dev-only crate {to}")]
    ShippingDependsOnDevTool { from: String, to: String },
    #[error("ARCH_BUILD_SCRIPT: unapproved build script in {0}")]
    UnapprovedBuildScript(String),
    #[error("ARCH_CYCLE: internal dependency cycle {0:?}")]
    Cycle(Vec<String>),
    #[error("ARCH_UNKNOWN_PACKAGE: policy references missing workspace package {0}")]
    UnknownWorkspacePackage(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_rejects_unknown_fields() {
        let policy = r#"
schema_version = "1.0"
workspace_members = ["xtask"]
dev_only_members = ["xtask"]
unexpected = true

[[crates]]
name = "xtask"
allowed_internal_dependencies = []
allow_build_script = false
"#;

        assert!(toml::from_str::<ArchitecturePolicy>(policy).is_err());
    }
}
