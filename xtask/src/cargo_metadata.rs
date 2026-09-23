//! Reads Cargo's versioned metadata JSON without a metadata-client dependency.
//!
//! Keep only fields used by repository checks; unexpected fields are harmless,
//! while malformed or unknown dependency kinds fail the check.

use std::{
    collections::BTreeSet,
    env,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Deserializer};

#[derive(Debug, Deserialize)]
pub(crate) struct Metadata {
    pub(crate) packages: Vec<Package>,
    pub(crate) workspace_members: Vec<String>,
    pub(crate) resolve: Option<Resolve>,
    pub(crate) workspace_root: PathBuf,
    version: u32,
}

impl Metadata {
    pub(crate) fn workspace_packages(&self) -> Vec<&Package> {
        let members: BTreeSet<&str> = self.workspace_members.iter().map(String::as_str).collect();
        self.packages
            .iter()
            .filter(|package| members.contains(package.id.as_str()))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn root_package(&self) -> Option<&Package> {
        let manifest = self.workspace_root.join("Cargo.toml");
        self.packages
            .iter()
            .find(|package| package.manifest_path == manifest)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct Resolve {
    pub(crate) nodes: Vec<ResolveNode>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResolveNode {
    pub(crate) id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Package {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) source: Option<String>,
    pub(crate) manifest_path: PathBuf,
    pub(crate) license: Option<String>,
    pub(crate) dependencies: Vec<Dependency>,
    pub(crate) targets: Vec<Target>,
    pub(crate) links: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Dependency {
    pub(crate) name: String,
    pub(crate) source: Option<String>,
    pub(crate) path: Option<PathBuf>,
    pub(crate) req: String,
    #[serde(default, deserialize_with = "dependency_kind")]
    pub(crate) kind: DependencyKind,
    pub(crate) optional: bool,
    pub(crate) uses_default_features: bool,
    pub(crate) rename: Option<String>,
    pub(crate) target: Option<String>,
    pub(crate) registry: Option<String>,
    pub(crate) features: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize)]
pub(crate) enum DependencyKind {
    #[default]
    Normal,
    #[serde(rename = "dev")]
    Development,
    #[serde(rename = "build")]
    Build,
}

fn dependency_kind<'de, D>(deserializer: D) -> Result<DependencyKind, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<DependencyKind>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
pub(crate) struct Target {
    pub(crate) kind: Vec<String>,
    pub(crate) name: String,
    pub(crate) src_path: PathBuf,
}

#[derive(Default)]
pub(crate) struct MetadataCommand {
    manifest_path: Option<PathBuf>,
    current_dir: Option<PathBuf>,
    no_deps: bool,
    all_features: bool,
    locked: bool,
}

impl MetadataCommand {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn manifest_path(mut self, path: impl AsRef<Path>) -> Self {
        self.manifest_path = Some(path.as_ref().to_path_buf());
        self
    }

    pub(crate) fn current_dir(mut self, path: impl AsRef<Path>) -> Self {
        self.current_dir = Some(path.as_ref().to_path_buf());
        self
    }

    pub(crate) fn no_deps(mut self) -> Self {
        self.no_deps = true;
        self
    }

    pub(crate) fn all_features(mut self) -> Self {
        self.all_features = true;
        self
    }

    pub(crate) fn locked(mut self) -> Self {
        self.locked = true;
        self
    }

    pub(crate) fn exec(self) -> Result<Metadata, MetadataError> {
        let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command.args(["metadata", "--format-version=1", "--offline"]);
        if self.no_deps {
            command.arg("--no-deps");
        }
        if self.all_features {
            command.arg("--all-features");
        }
        if self.locked {
            command.arg("--locked");
        }
        if let Some(path) = self.manifest_path {
            command.arg("--manifest-path").arg(path);
        }
        if let Some(path) = self.current_dir {
            command.current_dir(path);
        }
        let output = command.output().map_err(MetadataError::Spawn)?;
        if !output.status.success() {
            return Err(MetadataError::Cargo {
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(4096)
                    .collect(),
            });
        }
        let metadata: Metadata =
            serde_json::from_slice(&output.stdout).map_err(MetadataError::Decode)?;
        if metadata.version != 1 {
            return Err(MetadataError::Version(metadata.version));
        }
        Ok(metadata)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MetadataError {
    #[error("failed to run cargo metadata")]
    Spawn(#[source] std::io::Error),
    #[error("cargo metadata failed with {status}: {stderr}")]
    Cargo { status: String, stderr: String },
    #[error("invalid cargo metadata JSON")]
    Decode(#[source] serde_json::Error),
    #[error("unsupported cargo metadata format version {0}")]
    Version(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_kind_rejects_unknown_values() {
        let dependency = serde_json::json!({
            "name": "fixture", "source": null, "path": null, "req": "=1.0.0",
            "kind": "unreviewed", "optional": false, "uses_default_features": false,
            "rename": null, "target": null, "registry": null, "features": []
        });
        assert!(serde_json::from_value::<Dependency>(dependency).is_err());
    }

    #[test]
    fn live_metadata_contains_workspace_and_resolved_packages() {
        let metadata = MetadataCommand::new()
            .locked()
            .exec()
            .expect("Cargo metadata loads offline");
        assert!(!metadata.workspace_packages().is_empty());
        assert!(
            metadata
                .resolve
                .as_ref()
                .is_some_and(|resolve| !resolve.nodes.is_empty())
        );
    }
}
