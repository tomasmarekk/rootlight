//! Immutable benchmark dataset validation and cache acquisition.
//!
//! Acquisition copies bounded regular files in canonical order, never executes
//! dataset content, and emits source-free metadata tied to the source revision.

#![forbid(unsafe_code)]

use std::{
    fs::{self, File},
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;

const CATALOG_PATH: &str = "benchmarks/datasets.toml";
const CATALOG_SCHEMA: &str = "rootlight.benchmark-datasets/1";
const CACHE_SCHEMA: &str = "rootlight.benchmark-cache/1";
const TREE_DOMAIN: &[u8] = b"rootlight.dataset-tree/1\0";
const MAX_CATALOG_BYTES: u64 = 512 * 1024;
const EXPECTED_DATASETS: [&str; 3] = [
    "agent-budget-v1",
    "language-structural-v1",
    "vertical-slice-v1",
];

#[derive(Debug)]
pub(crate) struct CacheOptions {
    cache_dir: PathBuf,
    output: PathBuf,
    source_revision: String,
}

impl CacheOptions {
    pub(crate) fn parse(args: &mut impl Iterator<Item = String>) -> Result<Self, DatasetError> {
        let mut cache_dir = None;
        let mut output = None;
        let mut source_revision = None;
        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| DatasetError::MissingFlagValue(flag.clone()))?;
            match flag.as_str() {
                "--cache-dir" => {
                    assign_once(&mut cache_dir, PathBuf::from(value), "--cache-dir")?;
                }
                "--output" => assign_once(&mut output, PathBuf::from(value), "--output")?,
                "--source-revision" => {
                    assign_once(&mut source_revision, value, "--source-revision")?;
                }
                _ => return Err(DatasetError::UnexpectedArgument(flag)),
            }
        }
        Ok(Self {
            cache_dir: cache_dir.ok_or(DatasetError::MissingRequiredFlag("--cache-dir"))?,
            output: output.ok_or(DatasetError::MissingRequiredFlag("--output"))?,
            source_revision: source_revision
                .ok_or(DatasetError::MissingRequiredFlag("--source-revision"))?,
        })
    }
}

pub(crate) fn check() -> Result<(), DatasetError> {
    let workspace = workspace_root()?;
    let catalog = load_catalog(&workspace)?;
    let observations = validate_catalog(&workspace, &catalog)?;
    println!(
        "dataset contract passed for {} immutable inputs and {} bytes",
        observations.len(),
        observations
            .iter()
            .map(|observation| observation.bytes)
            .sum::<u64>()
    );
    Ok(())
}

pub(crate) fn acquire(options: &CacheOptions) -> Result<(), DatasetError> {
    validate_source_revision(&options.source_revision)?;
    let workspace = workspace_root()?;
    let catalog = load_catalog(&workspace)?;
    let observations = validate_catalog(&workspace, &catalog)?;
    prepare_new_directory(&options.cache_dir)?;
    for (dataset, observation) in catalog.datasets.iter().zip(&observations) {
        copy_dataset(
            &workspace.join(&dataset.source),
            &options.cache_dir.join(&dataset.cache_path),
            observation,
        )?;
    }
    let report = CacheManifest {
        schema: &catalog.cache_schema,
        source_revision: &options.source_revision,
        catalog_sha256: sha256(&read_regular_bounded(
            &workspace.join(CATALOG_PATH),
            MAX_CATALOG_BYTES,
        )?),
        acquisition: "workspace_copy",
        executed_repository_content: false,
        datasets: catalog
            .datasets
            .iter()
            .zip(&observations)
            .map(|(dataset, observation)| CacheDataset {
                id: &dataset.id,
                source: &dataset.source,
                pin: &dataset.pin,
                tree_sha256: &observation.tree_sha256,
                license: &dataset.license,
                scope: &dataset.scope,
                generated_policy: &dataset.generated_policy,
                cache_path: &dataset.cache_path,
                file_count: observation.files.len(),
                bytes: observation.bytes,
                files: &observation.files,
            })
            .collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&report).map_err(DatasetError::SerializeManifest)?;
    bytes.push(b'\n');
    persist_new_file(&options.output, &bytes)?;
    println!(
        "acquired {} immutable datasets into a source-local cache",
        observations.len()
    );
    Ok(())
}

fn assign_once<T>(slot: &mut Option<T>, value: T, flag: &'static str) -> Result<(), DatasetError> {
    if slot.replace(value).is_some() {
        return Err(DatasetError::DuplicateFlag(flag));
    }
    Ok(())
}

fn workspace_root() -> Result<PathBuf, DatasetError> {
    let mut candidate = std::env::current_dir().map_err(DatasetError::WorkingDir)?;
    for _ in 0..8 {
        if candidate.join("Cargo.toml").is_file() && candidate.join(CATALOG_PATH).is_file() {
            return Ok(candidate);
        }
        if !candidate.pop() {
            break;
        }
    }
    Err(DatasetError::InvalidCatalog(
        "run dataset tooling from within the workspace".to_owned(),
    ))
}

fn load_catalog(workspace: &Path) -> Result<DatasetCatalog, DatasetError> {
    let path = workspace.join(CATALOG_PATH);
    let bytes = read_regular_bounded(&path, MAX_CATALOG_BYTES)?;
    let text = std::str::from_utf8(&bytes).map_err(DatasetError::InvalidUtf8)?;
    toml::from_str(text).map_err(DatasetError::ParseCatalog)
}

fn validate_catalog(
    workspace: &Path,
    catalog: &DatasetCatalog,
) -> Result<Vec<DatasetObservation>, DatasetError> {
    if catalog.schema != CATALOG_SCHEMA {
        return invalid_catalog(format!("schema must be {CATALOG_SCHEMA}"));
    }
    if catalog.cache_schema != CACHE_SCHEMA {
        return invalid_catalog(format!("cache schema must be {CACHE_SCHEMA}"));
    }
    if !(1024..=u64::from(u32::MAX)).contains(&catalog.maximum_file_bytes)
        || catalog.maximum_dataset_bytes < catalog.maximum_file_bytes
        || catalog.maximum_dataset_bytes > 4 * 1024 * 1024 * 1024
        || !(1..=16_384).contains(&catalog.maximum_files)
        || !(1..=64).contains(&catalog.maximum_depth)
    {
        return invalid_catalog("dataset resource limits are invalid");
    }
    let ids = catalog
        .datasets
        .iter()
        .map(|dataset| dataset.id.as_str())
        .collect::<Vec<_>>();
    if ids != EXPECTED_DATASETS {
        return invalid_catalog("dataset inventory must be sorted and complete");
    }

    let mut observations = Vec::with_capacity(catalog.datasets.len());
    for dataset in &catalog.datasets {
        validate_token(&dataset.id)?;
        validate_relative_path(&dataset.source)?;
        validate_relative_path(&dataset.cache_path)?;
        if !dataset.cache_path.starts_with("datasets/")
            || !dataset.cache_path.ends_with(&dataset.id)
        {
            return invalid_catalog(format!(
                "{} cache path must be namespaced by its identifier",
                dataset.id
            ));
        }
        validate_digest(&dataset.tree_sha256)?;
        if dataset.pin != format!("tree-sha256:{}", dataset.tree_sha256) {
            return invalid_catalog(format!("{} pin must bind its tree digest", dataset.id));
        }
        if dataset.license != "AGPL-3.0-only" {
            return invalid_catalog(format!("{} has an unsupported license", dataset.id));
        }
        validate_token(&dataset.scope)?;
        if dataset.generated_policy != "exclude_generated_files" {
            return invalid_catalog(format!(
                "{} must explicitly exclude generated files",
                dataset.id
            ));
        }
        if dataset.acquisition != "workspace_copy" {
            return invalid_catalog(format!(
                "{} has a non-reproducible acquisition mode",
                dataset.id
            ));
        }
        let observation = observe_tree(
            &workspace.join(&dataset.source),
            catalog.maximum_file_bytes,
            catalog.maximum_dataset_bytes,
            catalog.maximum_files,
            catalog.maximum_depth,
        )?;
        if observation.tree_sha256 != dataset.tree_sha256 {
            return Err(DatasetError::TreeDigestMismatch {
                dataset: dataset.id.clone(),
                expected: dataset.tree_sha256.clone(),
                observed: observation.tree_sha256,
            });
        }
        observations.push(observation);
    }
    Ok(observations)
}

fn observe_tree(
    root: &Path,
    maximum_file_bytes: u64,
    maximum_dataset_bytes: u64,
    maximum_files: usize,
    maximum_depth: usize,
) -> Result<DatasetObservation, DatasetError> {
    let metadata = fs::symlink_metadata(root).map_err(|error| DatasetError::FileIo {
        path: root.to_path_buf(),
        error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return invalid_catalog(format!(
            "{} must be a non-symlink directory",
            root.display()
        ));
    }
    let mut paths = Vec::new();
    collect_paths(root, root, 0, maximum_depth, maximum_files, &mut paths)?;
    paths.sort_by(|left, right| left.0.cmp(&right.0));
    if paths.is_empty() {
        return invalid_catalog(format!("{} contains no files", root.display()));
    }

    let mut tree = Sha256::new();
    tree.update(TREE_DOMAIN);
    let mut files = Vec::with_capacity(paths.len());
    let mut total_bytes = 0_u64;
    for (relative, path) in paths {
        let bytes = read_regular_bounded(&path, maximum_file_bytes)?;
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        total_bytes = total_bytes
            .checked_add(size)
            .ok_or(DatasetError::DatasetTooLarge {
                path: root.to_path_buf(),
                maximum: maximum_dataset_bytes,
            })?;
        if total_bytes > maximum_dataset_bytes {
            return Err(DatasetError::DatasetTooLarge {
                path: root.to_path_buf(),
                maximum: maximum_dataset_bytes,
            });
        }
        let file_digest = Sha256::digest(&bytes);
        tree.update(
            u64::try_from(relative.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        tree.update(relative.as_bytes());
        tree.update(size.to_be_bytes());
        tree.update(file_digest);
        files.push(CacheFile {
            path: relative,
            bytes: size,
            sha256: data_encoding::HEXLOWER.encode(&file_digest),
        });
    }
    Ok(DatasetObservation {
        tree_sha256: data_encoding::HEXLOWER.encode(&tree.finalize()),
        bytes: total_bytes,
        files,
    })
}

fn collect_paths(
    root: &Path,
    directory: &Path,
    depth: usize,
    maximum_depth: usize,
    maximum_files: usize,
    output: &mut Vec<(String, PathBuf)>,
) -> Result<(), DatasetError> {
    if depth > maximum_depth {
        return Err(DatasetError::TreeDepthExceeded {
            path: directory.to_path_buf(),
            maximum: maximum_depth,
        });
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|error| DatasetError::FileIo {
            path: directory.to_path_buf(),
            error,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| DatasetError::FileIo {
            path: directory.to_path_buf(),
            error,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| DatasetError::FileIo {
            path: path.clone(),
            error,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(DatasetError::UnsupportedEntry {
                path,
                kind: "symlink",
            });
        }
        if metadata.is_dir() {
            collect_paths(
                root,
                &path,
                depth.saturating_add(1),
                maximum_depth,
                maximum_files,
                output,
            )?;
        } else if metadata.is_file() {
            if output.len() >= maximum_files {
                return Err(DatasetError::FileCountExceeded {
                    path: root.to_path_buf(),
                    maximum: maximum_files,
                });
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| DatasetError::UnsupportedEntry {
                    path: path.clone(),
                    kind: "escaped path",
                })?;
            let relative = normalized_relative(relative)?;
            output.push((relative, path));
        } else {
            return Err(DatasetError::UnsupportedEntry {
                path,
                kind: "non-file entry",
            });
        }
    }
    Ok(())
}

fn copy_dataset(
    source_root: &Path,
    cache_root: &Path,
    observation: &DatasetObservation,
) -> Result<(), DatasetError> {
    if cache_root.exists() {
        return Err(DatasetError::OutputExists(cache_root.to_path_buf()));
    }
    fs::create_dir_all(cache_root).map_err(|error| DatasetError::FileIo {
        path: cache_root.to_path_buf(),
        error,
    })?;
    for file in &observation.files {
        let source = source_root.join(&file.path);
        let bytes = read_regular_bounded(&source, file.bytes)?;
        if sha256(&bytes) != file.sha256 {
            return Err(DatasetError::SourceChanged(source));
        }
        let destination = cache_root.join(&file.path);
        let parent = destination.parent().ok_or_else(|| {
            DatasetError::InvalidCatalog("cache destination has no parent".to_owned())
        })?;
        fs::create_dir_all(parent).map_err(|error| DatasetError::FileIo {
            path: parent.to_path_buf(),
            error,
        })?;
        fs::write(&destination, bytes).map_err(|error| DatasetError::FileIo {
            path: destination,
            error,
        })?;
    }
    Ok(())
}

fn prepare_new_directory(path: &Path) -> Result<(), DatasetError> {
    if path.exists() {
        return Err(DatasetError::OutputExists(path.to_path_buf()));
    }
    fs::create_dir_all(path).map_err(|error| DatasetError::FileIo {
        path: path.to_path_buf(),
        error,
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|error| DatasetError::FileIo {
        path: path.to_path_buf(),
        error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return invalid_catalog("cache root must be a non-symlink directory");
    }
    Ok(())
}

fn persist_new_file(path: &Path, bytes: &[u8]) -> Result<(), DatasetError> {
    if path.exists() {
        return Err(DatasetError::OutputExists(path.to_path_buf()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| DatasetError::InvalidCatalog("manifest output has no parent".to_owned()))?;
    fs::create_dir_all(parent).map_err(|error| DatasetError::FileIo {
        path: parent.to_path_buf(),
        error,
    })?;
    let metadata = fs::symlink_metadata(parent).map_err(|error| DatasetError::FileIo {
        path: parent.to_path_buf(),
        error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return invalid_catalog("manifest parent must be a non-symlink directory");
    }
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| DatasetError::FileIo {
        path: parent.to_path_buf(),
        error,
    })?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|error| DatasetError::FileIo {
            path: path.to_path_buf(),
            error,
        })?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| DatasetError::FileIo {
            path: path.to_path_buf(),
            error: error.error,
        })?;
    Ok(())
}

fn read_regular_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, DatasetError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| DatasetError::FileIo {
        path: path.to_path_buf(),
        error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DatasetError::UnsupportedEntry {
            path: path.to_path_buf(),
            kind: "non-regular file",
        });
    }
    if metadata.len() > maximum {
        return Err(DatasetError::FileTooLarge {
            path: path.to_path_buf(),
            maximum,
        });
    }
    let file = File::open(path).map_err(|error| DatasetError::FileIo {
        path: path.to_path_buf(),
        error,
    })?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| DatasetError::FileIo {
            path: path.to_path_buf(),
            error,
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(DatasetError::FileTooLarge {
            path: path.to_path_buf(),
            maximum,
        });
    }
    Ok(bytes)
}

fn normalized_relative(path: &Path) -> Result<String, DatasetError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return invalid_catalog("dataset path is not canonical");
        };
        let part = part
            .to_str()
            .ok_or_else(|| DatasetError::InvalidCatalog("dataset path is not UTF-8".to_owned()))?;
        parts.push(part);
    }
    let normalized = parts.join("/");
    validate_relative_path(&normalized)?;
    Ok(normalized)
}

fn validate_relative_path(value: &str) -> Result<(), DatasetError> {
    if value.is_empty()
        || value.len() > 240
        || value.contains('\\')
        || value.contains(':')
        || value.contains("//")
    {
        return invalid_catalog(format!("invalid relative path {value:?}"));
    }
    for component in Path::new(value).components() {
        if !matches!(component, Component::Normal(_)) {
            return invalid_catalog(format!("invalid relative path {value:?}"));
        }
    }
    Ok(())
}

fn validate_token(value: &str) -> Result<(), DatasetError> {
    if value.is_empty()
        || value.len() > 96
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        return invalid_catalog(format!("invalid dataset token {value:?}"));
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), DatasetError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return invalid_catalog("dataset digest must be lowercase SHA-256");
    }
    Ok(())
}

fn validate_source_revision(value: &str) -> Result<(), DatasetError> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(DatasetError::InvalidSourceRevision);
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(bytes))
}

fn invalid_catalog<T>(detail: impl Into<String>) -> Result<T, DatasetError> {
    Err(DatasetError::InvalidCatalog(detail.into()))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DatasetCatalog {
    schema: String,
    cache_schema: String,
    maximum_dataset_bytes: u64,
    maximum_file_bytes: u64,
    maximum_files: usize,
    maximum_depth: usize,
    datasets: Vec<DatasetSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DatasetSpec {
    id: String,
    source: String,
    pin: String,
    tree_sha256: String,
    license: String,
    scope: String,
    generated_policy: String,
    acquisition: String,
    cache_path: String,
}

#[derive(Debug)]
struct DatasetObservation {
    tree_sha256: String,
    bytes: u64,
    files: Vec<CacheFile>,
}

#[derive(Debug, Serialize)]
struct CacheManifest<'a> {
    schema: &'a str,
    source_revision: &'a str,
    catalog_sha256: String,
    acquisition: &'static str,
    executed_repository_content: bool,
    datasets: Vec<CacheDataset<'a>>,
}

#[derive(Debug, Serialize)]
struct CacheDataset<'a> {
    id: &'a str,
    source: &'a str,
    pin: &'a str,
    tree_sha256: &'a str,
    license: &'a str,
    scope: &'a str,
    generated_policy: &'a str,
    cache_path: &'a str,
    file_count: usize,
    bytes: u64,
    files: &'a [CacheFile],
}

#[derive(Clone, Debug, Serialize)]
struct CacheFile {
    path: String,
    bytes: u64,
    sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DatasetError {
    #[error("failed to determine the working directory")]
    WorkingDir(#[source] std::io::Error),
    #[error("dataset file {path} could not be accessed")]
    FileIo {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("benchmark dataset catalog is not valid TOML")]
    ParseCatalog(#[source] toml::de::Error),
    #[error("benchmark dataset catalog must be UTF-8")]
    InvalidUtf8(#[source] std::str::Utf8Error),
    #[error("benchmark dataset catalog is invalid: {0}")]
    InvalidCatalog(String),
    #[error("dataset {dataset} tree digest differs: expected {expected}, observed {observed}")]
    TreeDigestMismatch {
        dataset: String,
        expected: String,
        observed: String,
    },
    #[error("dataset file {path} exceeds {maximum} bytes")]
    FileTooLarge { path: PathBuf, maximum: u64 },
    #[error("dataset tree {path} exceeds {maximum} bytes")]
    DatasetTooLarge { path: PathBuf, maximum: u64 },
    #[error("dataset tree {path} exceeds {maximum} files")]
    FileCountExceeded { path: PathBuf, maximum: usize },
    #[error("dataset tree depth at {path} exceeds {maximum}")]
    TreeDepthExceeded { path: PathBuf, maximum: usize },
    #[error("dataset entry {path} has unsupported kind {kind}")]
    UnsupportedEntry { path: PathBuf, kind: &'static str },
    #[error("dataset source changed during acquisition: {0}")]
    SourceChanged(PathBuf),
    #[error("dataset cache manifest serialization failed")]
    SerializeManifest(#[source] serde_json::Error),
    #[error("source revision must be a canonical 40- or 64-character lowercase hex digest")]
    InvalidSourceRevision,
    #[error("required argument {0} is missing")]
    MissingRequiredFlag(&'static str),
    #[error("argument {0} requires a value")]
    MissingFlagValue(String),
    #[error("argument {0} was provided more than once")]
    DuplicateFlag(&'static str),
    #[error("unexpected argument: {0}")]
    UnexpectedArgument(String),
    #[error("immutable dataset output already exists: {0}")]
    OutputExists(PathBuf),
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn catalog_binds_every_dataset_tree() {
        let workspace = workspace_root().expect("workspace root");
        let catalog = load_catalog(&workspace).expect("catalog parses");

        let observations = validate_catalog(&workspace, &catalog).expect("catalog validates");

        assert_eq!(observations.len(), EXPECTED_DATASETS.len());
        assert!(observations.iter().all(|dataset| dataset.bytes > 0));
    }

    #[test]
    fn changed_pin_is_rejected() {
        let workspace = workspace_root().expect("workspace root");
        let mut catalog = load_catalog(&workspace).expect("catalog parses");
        catalog.datasets[0].pin =
            "tree-sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
                .to_owned();

        assert!(validate_catalog(&workspace, &catalog).is_err());
    }

    #[test]
    fn tree_hash_changes_with_file_content() {
        let tree = tempdir().expect("tree");
        fs::write(tree.path().join("fixture.rs"), b"fn first() {}\n").expect("first fixture");
        let first = observe_tree(tree.path(), 1024, 4096, 8, 4).expect("first tree");
        fs::write(tree.path().join("fixture.rs"), b"fn second() {}\n").expect("second fixture");
        let second = observe_tree(tree.path(), 1024, 4096, 8, 4).expect("second tree");

        assert_ne!(first.tree_sha256, second.tree_sha256);
    }

    #[test]
    fn cache_acquisition_is_deterministic() {
        let output = tempdir().expect("output");
        let first = CacheOptions {
            cache_dir: output.path().join("cache-first"),
            output: output.path().join("manifest-first.json"),
            source_revision: "1111111111111111111111111111111111111111".to_owned(),
        };
        let second = CacheOptions {
            cache_dir: output.path().join("cache-second"),
            output: output.path().join("manifest-second.json"),
            source_revision: first.source_revision.clone(),
        };

        acquire(&first).expect("first acquisition");
        acquire(&second).expect("second acquisition");

        assert_eq!(
            fs::read(&first.output).expect("first manifest"),
            fs::read(&second.output).expect("second manifest")
        );
        compare_cache_trees(&first.cache_dir, &second.cache_dir).expect("cache trees match");
    }

    fn compare_cache_trees(first: &Path, second: &Path) -> Result<(), DatasetError> {
        let left = observe_tree(first, 16 * 1024 * 1024, 512 * 1024 * 1024, 4096, 32)?;
        let right = observe_tree(second, 16 * 1024 * 1024, 512 * 1024 * 1024, 4096, 32)?;
        if left.tree_sha256 != right.tree_sha256 {
            return invalid_catalog("cache tree digests differ");
        }
        Ok(())
    }
}
